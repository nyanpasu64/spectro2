# spectro2

spectro2 is an in-development audio spectrum visualizer which shows precise pitches, and visualizes phase as well as amplitude.

spectro2 is written in Rust, performs frequency-domain analysis (FFT) on the CPU, and renders an image on the GPU using wgpu.

## Building

Clone the repo and run `cargo run`. `cargo run --release` will not generate debug info, and may or may not produce a slightly faster binary.

Note that this project has custom flags for debug and release builds. Dependencies like the FFT algorithm are compiled in `-O2` in both debug and release mode; only this crate has optimization disabled in debug mode. See `.cargo/config.toml` for details.

## Usage

If you type `cargo run [...] --`, all arguments after the double-hyphen are passed to `spectro2` instead of `cargo run`.

Example usage: `cargo run -- --loopback --volume 100`

**SEIZURE WARNING:** Rapidly changing audio can cause flashing lights, especially once colored stereo is added.

Full docs: `cargo run -- --help` (`-h` will only print short help).

```
USAGE:
    spectro2.exe [FLAGS] [OPTIONS]

OPTIONS:
    -l, --loopback
            If passed, will listen to output device (speaker) instead of input (microphone)

    -d, --device-index <device-index>
            If passed, will override which device is selected.

            This overrides --loopback for picking devices. However, you still need to pass --loopback if you pass an
            output device (speaker) to --device-index.
    -v, --volume <volume>
            How much to amplify the incoming signal before sending it to the spectrum viewer [default: 20]

    -f, --fft-size <fft-size>
            Number of samples to use in each FFT block.

            Increasing this value makes it easier to identify pitches, but increases audio latency and smearing in time.
            Must be a multiple of --redraw-size. [default: 2048]
    -r, --redraw-size <redraw-size>
            Number of samples to advance time before recalculating FFT.

            Decreasing this value causes FFTs to be computed more often, increasing CPU usage but reducing latency and
            stuttering.

            If this value exceeds --fft-size, it is clamped to it. Otherwise must be a factor of --fft-size. [default:
            512]
        --fps <fps>
            Limit the FPS of the rendering thread.

            If set to 0, FPS is unbounded and this program will max out the CPU and/or GPU.

            This program does not support vsync because it adds around 3 frames of latency. [default: 200]
        --print-fps
            If passed, prints FPS to the terminal

    -h, --help
            Prints help information

    -V, --version
            Prints version information
```

Note that loopback mode has somewhat higher latency than microphone input, to the point it can distract from listening to music. I'm not familiar with latency compensation, and if anyone has suggestions, feel free to let me know. My best attempt so far is to route your music player through VB-Audio Virtual Cable's input, use `--device-index` to visualize it, and configure Windows to listen to the output. This will delay audio by more than the video latency, which may be worse than not using it.

## Documentation

There are (internal, disorganized) design documents (planning and notes) at https://drive.google.com/drive/folders/1Jzo9SQ8yuVD9YK7dTZaC7mlJmPSnHnZy?usp=sharing.

## Credits

This program would not be possible without the assistance of:

- The Rust Community Discord, #games-and-graphics
- https://sotrh.github.io/learn-wgpu/
