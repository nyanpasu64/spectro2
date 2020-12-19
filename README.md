# spectro2

spectro2 is an in-development audio spectrum visualizer which shows precise pitches, and visualizes phase as well as amplitude.

spectro2 is written in Rust, performs frequency-domain analysis (FFT) on the CPU, and renders an image on the GPU using wgpu.

## Building

Clone the repo and run `cargo run`. `cargo run --release` will not generate debug info, and may or may not produce a slightly faster binary.

Note that this project has custom flags for debug and release builds. Dependencies like the FFT algorithm are compiled in `-O2` in both debug and release mode; only this crate has optimization disabled in debug mode. See `.cargo/config.toml` for details.

## Usage

If you type `cargo run [...] --`, all arguments after the double-hyphen are passed to `spectro2` instead of `cargo run`.

Example usage: `cargo run -- --loopback`

Full docs: `cargo run -- --help`

```
USAGE:
    spectro2.exe [FLAGS] [OPTIONS]

FLAGS:
    -h, --help
            Prints help information

    -l, --loopback
            If passed, will listen to speaker instead of microphone. Note that this causes substantial latency (around
            180ms), and you may wish to route speakers through VB-Audio Virtual Cable so both speakers and the
            visualization are delayed by the same amount
    -V, --version
            Prints version information


OPTIONS:
    -f, --fft-size <fft-size>
            Number of samples to use in each FFT block. Increasing this value makes it easier to identify pitches, but
            increases audio latency and smearing in time. Must be a multiple of --redraw-size [default: 2048]
    -r, --redraw-size <redraw-size>
            Number of samples to advance time before recalculating FFT. Decreasing this value causes FFTs to be computed
            more often, increasing CPU usage but reducing latency and stuttering.

            If this value exceeds --fft-size, it is clamped to it. Otherwise must be a factor of --fft-size. [default:
            512]
    -v, --volume <volume>
            How much to amplify the incoming signal before sending it to the spectrum viewer [default: 20]
```

## Documentation

There are (internal, disorganized) design documents (planning and notes) at https://drive.google.com/drive/folders/1Jzo9SQ8yuVD9YK7dTZaC7mlJmPSnHnZy?usp=sharing.

## Credits

This program would not be possible without the assistance of:

- The Rust Community Discord, #games-and-graphics
- https://sotrh.github.io/learn-wgpu/
