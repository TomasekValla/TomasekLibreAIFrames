# How TomasekLibreAIFrames works

This document explains, in plain words, how the app turns a video into a smoother (more frames per second)
and/or sharper (higher resolution) one, why it is built this way, and where its limits are.

None of the AI models are ours - RIFE, AnimeJaNai, Real-CUGAN, RealPLKSR and the others are the work of their
authors. What this project adds is the plumbing around them: getting frames in and out quickly, running the
models on almost any computer, and not breaking when a driver or a GPU misbehaves.

## The short version

1. **FFmpeg decodes** the video into raw pixels and pipes them straight into the app's memory.
2. **AI models run on the GPU** through ONNX Runtime: an upscaler makes each frame bigger, RIFE invents the
   frames in between.
3. **FFmpeg encodes** the result as it comes out, and copies the original audio and subtitles.

Nothing is written to disk in between. A 4K video never becomes tens of gigabytes of PNG files, and the output
starts growing seconds after you press Start.

```
 video file -> ffmpeg decode -> [upscale workers] -> [interpolation workers] -> ffmpeg encode -> output
                 (raw pixels)        stage A               stage B                (GPU or CPU)
```

## Why it is built this way

### 1. Frames stream through memory, like an assembly line

Older tools (and our own first script) worked in three separate passes: extract every frame to PNG files,
run the AI over the folder, then encode the folder back into a video. That needs a lot of disk space and the
encoder has to wait for the AI to finish everything.

Here each frame walks through the stations one after another. While the encoder writes frame 10, the
interpolator works on frame 11 and the upscaler on frame 12. Order is guaranteed by bounded queues between the
stations: a station can only take a job when the frames it depends on are finished. Memory use stays flat no
matter how long the video is.

### 2. Several model "streams" keep the GPU busy

A GPU computes fast, but somebody has to prepare its input first (copy pixels, pad the edges, convert formats)
and that happens on the CPU. With one model instance the GPU waits during that time.

The app therefore runs **two copies of each model in parallel** (configurable). While one copy is being fed by
the CPU, the other is computing. Results are put back into the original order before encoding.

Measured on a 4K clip, 2x interpolation, RIFE 4.26 Heavy, DirectML, a 16 GB desktop GPU on Windows 11:

| Model streams | Time | GPU memory |
|---|---|---|
| 1 | 38.2 s | 4.0 GB |
| **2** | **25.6 s** | 8.0 GB |
| 3 | 48.6 s | 12.1 GB |
| 4 | 122 s | 16.1 GB (full - spills into system RAM) |

GPU utilisation went from about 53 % to about 81 %. More than two streams made things slower once the GPU memory
was full, so "Auto" uses two streams, or one when two would not fit into GPU memory.

### 3. One app, many GPUs: ONNX Runtime with the right backend per system

All models are used in the ONNX format and executed by ONNX Runtime, which can talk to very different hardware:

| System | Backend | Works with |
|---|---|---|
| Windows | DirectML | any DirectX 12 GPU (AMD, NVIDIA, Intel) |
| macOS | CoreML | Apple Silicon GPU and Neural Engine |
| Linux | WebGPU / CUDA / MIGraphX | any Vulkan GPU / NVIDIA / AMD ROCm |
| everywhere | CPU | slow, but always works |

If a GPU backend cannot load a model or fails while running (for example out of memory), the app reloads the
model on the CPU and continues - and tells you, because it will be much slower.

### 4. A silent DirectML bug, found and worked around

During testing, RIFE on DirectML produced broken frames (ghosting and vertical stripes) for 4K input - with no
error message at all. Comparing the same frame computed on the CPU showed:

- 1080p input, or 4K split into tiles up to 2048x2048: identical to the CPU result.
- Larger single passes: wrong output.

The limit matches an internal tensor reaching 2^31 bytes. The app now splits frames above about 2048x2048 into
the fewest overlapping tiles (4K becomes three vertical strips) on DirectML and WebGPU. Only the centre of each
tile is kept, so the seams are invisible: the 4K result differs from the CPU result by at most 1/255 per pixel.

### 5. Exact timing for any frame rate

For every output frame the app computes where it falls between two source frames, using exact fractions
(for example 24000/1001 fps). That is why x2, x3.5, 59.94 fps or 5000 fps all work the same way, and why the
output stays in sync with the original audio.

**Scene cuts** are detected by comparing small thumbnails of neighbouring frames. Across a cut, the app repeats
the nearest real frame instead of blending two unrelated shots into a smear.

### 6. The faster order is picked automatically

When upscaling and interpolating together, there are two possible orders:

- upscale first: every source frame is upscaled once, then RIFE works at the high resolution;
- interpolate first: RIFE works at the small resolution, then every output frame is upscaled.

Which one is faster depends on the models, the resolution and the frame-rate factor. The app estimates both from
the models' speed (measured by its benchmark when available) and shows the choice and the reason.

### 7. Encoding never fails because of one driver

Before each job, the app test-encodes two frames at the real output size with the chosen encoder. If that fails
(old driver, no hardware support, or a size the GPU encoder cannot handle - most top out around 8K), it moves on
to the next GPU encoder and finally to the CPU (x264, x265, SVT-AV1, with AVX-512 where the CPU has it).

### 8. Honest model ranking

- **Quality** is a curated score from public comparisons. It is an estimate, and the app says so.
- **Speed and memory** are measured on your own computer with the Benchmark button. Until then they are marked
  "est.".
- Every model shows its license and links to its source. Models are downloaded from their authors' releases and
  checked by SHA-256. They are not part of this repository.

## Known limits

- RIFE is not magic: very fast motion (for example a close-up turning quickly) can still show ghosting in the
  invented frames. Every frame-interpolation model has this limit.
- DirectML is slower than vendor-specific runtimes such as TensorRT. On NVIDIA Linux, the CUDA build is faster.
- Linux and macOS builds are produced by CI; most testing so far happened on Windows.
- GPU encoders have size limits; very large outputs fall back to CPU encoders, which are much slower.

## Where things are in the code

| File | What it does |
|---|---|
| `src/pipeline.rs` | the assembly line: decoding, worker stages, ordering, encoding, benchmark, memory estimates |
| `src/infer.rs` | running a model: tiling, padding, CPU fallback, pixel conversion |
| `src/backend.rs` | which AI backends this build contains |
| `src/ffmpeg.rs` | probing, encoder detection and the fallback ladder |
| `src/models.rs` | model catalog, licenses, downloads |
| `src/app.rs`, `src/ui_*.rs` | the user interface |
| `src/cli.rs` | headless mode for scripts and tests |
