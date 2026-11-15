<p align="center">
    <img width="160" alt="Alacritty Kitty logo" src="extra/logo/alacritty-kitty.svg">
</p>

<h1 align="center">Alacritty Kitty</h1>

<p align="center">
  An unofficial fork of <a href="https://github.com/alacritty/alacritty">Alacritty</a>
  with support for the <a href="https://sw.kovidgoyal.net/kitty/graphics-protocol/">kitty graphics protocol</a>:
  images, animations and Unicode placeholders right in your terminal.
</p>

> [!IMPORTANT]
> This is **not** the official Alacritty and is not affiliated with or endorsed by
> the Alacritty project. Please report bugs of this fork
> [here](https://github.com/SatoKazuma1/alacritty-kitty/issues), not to Alacritty.
>
> The fork was developed with the help of an AI coding assistant (Claude) and
> tested by its maintainer on Linux. Upstream Alacritty doesn't accept LLM
> generated code, so none of these changes will be submitted upstream.

## Features

Everything Alacritty does, plus the kitty graphics protocol:

- **Transmission**: direct (chunked), file, temporary file and shared memory;
  RGB, RGBA and PNG data with optional zlib compression
- **Placements**: source rectangles, sizes in cells, pixel offsets, z-index in
  kitty's three layers (below cell backgrounds, below text, above text)
- **Unicode placeholders** (`U=1`), so images work inside tmux with
  `set -g allow-passthrough on`
- **Animation**: frame loading, editing and composition, terminal driven
  playback with gaps, loop counts and loading mode; frames are stored as deltas
- **Deletion** with all selectors, **queries** including feature queries
  (`e=`), responses honoring `q=`
- Erase in display behaves like kitty: `clear` removes visible images, partial
  erases keep them
- `CSI 16 t` cell size reports

Images are anchored to the line they were placed on, so they scroll with the
text and stay in the scrollback.

### Tested with

| Application | Notes |
|---|---|
| `kitten icat` | All transfer modes, animated GIFs |
| `chafa -f kitty` | Including animated GIFs |
| [yazi](https://github.com/sxyazi/yazi) | Image, PDF and video previews |
| [tdf](https://github.com/itsjunetime/tdf) | PDF viewer |
| `mpv --vo=kitty` | Video playback at full frame rate |
| `fastfetch` | `kitty` and `kitty-icat` logos (animated with `kitty-icat`) |
| tmux | With `allow-passthrough on`, through Unicode placeholders |

Tested on Arch Linux (CachyOS), KDE Plasma on Wayland with an NVIDIA GPU.
macOS and Windows builds are untested.

### Not supported

- Relative placements (`P`, `Q`, `H`, `V` keys)
- Indexed color images (`f=8`, `f=16`, added in kitty 0.50)
- Images aren't reflowed when the window width changes, like in kitty

## Installation

### Arch Linux

```sh
git clone https://github.com/SatoKazuma1/alacritty-kitty
cd alacritty-kitty/extra/arch
makepkg -si
```

This installs `alacritty-kitty` next to an existing Alacritty, with its own
desktop entry, icon and man pages (`man alacritty-kitty`).

### From source

Install the [build dependencies of Alacritty](INSTALL.md#dependencies), then:

```sh
cargo build --release
./target/release/alacritty
```

## Configuration

Alacritty Kitty reads the regular Alacritty configuration file. The graphics
protocol is enabled by default and can be configured in the `[terminal]`
section:

```toml
[terminal]
kitty_graphics = true          # Enable the kitty graphics protocol
graphics_memory_limit = 320    # MiB of image data before old images are evicted
```

## Branches

- `kitty-graphics`: the fork (default branch)
- `master`: mirror of upstream Alacritty

The protocol is implemented in `alacritty_terminal/src/graphics` and
`alacritty_terminal/src/term/kitty_graphics.rs`, rendering in
`alacritty/src/renderer/graphics.rs`. A patched copy of
[vte](https://github.com/alacritty/vte) in `vendor/vte` adds APC sequence
support.

## License

Alacritty Kitty is released under the [Apache License, Version 2.0], like
Alacritty. The vendored vte crate is licensed under Apache-2.0 or MIT.

---

# Alacritty

The rest of this document is the upstream Alacritty README.

## About

Alacritty is a modern terminal emulator that comes with sensible defaults, but
allows for extensive [configuration](#configuration). By integrating with other
applications, rather than reimplementing their functionality, it manages to
provide a flexible set of [features](./docs/features.md) with high performance.
The supported platforms currently consist of BSD, Linux, macOS and Windows.

The software is considered to be at a **beta** level of readiness; there are
a few missing features and bugs to be fixed, but it is already used by many as
a daily driver.

Precompiled binaries are available from the [GitHub releases page](https://github.com/alacritty/alacritty/releases).

Join [`#alacritty`] on libera.chat if you have questions or looking for a quick help.

[`#alacritty`]: https://web.libera.chat/gamja/?channels=#alacritty

## Features

You can find an overview over the features available in Alacritty [here](./docs/features.md).

## Further information

- [Announcing Alacritty, a GPU-Accelerated Terminal Emulator](https://jwilm.io/blog/announcing-alacritty/) January 6, 2017
- [A talk about Alacritty at the Rust Meetup January 2017](https://www.youtube.com/watch?v=qHOdYO3WUTk) January 19, 2017
- [Alacritty Lands Scrollback, Publishes Benchmarks](https://jwilm.io/blog/alacritty-lands-scrollback/) September 17, 2018

## Installation

Alacritty can be installed by using various package managers on Linux, BSD,
macOS and Windows.

Prebuilt binaries for macOS and Windows can also be downloaded from the
[GitHub releases page](https://github.com/alacritty/alacritty/releases).

For everyone else, the detailed instructions to install Alacritty can be found
[here](INSTALL.md).

### Requirements

- At least OpenGL ES 2.0
- [Windows] ConPTY support (Windows 10 version 1809 or higher)

## Configuration

You can find the documentation for Alacritty's configuration in `man 5
alacritty`, or by looking at [the website] if you do not have the manpages
installed.

[the website]: https://alacritty.org/config-alacritty.html

Alacritty doesn't create the config file for you, but it looks for one in the
following locations:

1. `$XDG_CONFIG_HOME/alacritty/alacritty.toml`
2. `$XDG_CONFIG_HOME/alacritty.toml`
3. `$HOME/.config/alacritty/alacritty.toml`
4. `$HOME/.alacritty.toml`
5. `/etc/alacritty/alacritty.toml`

On Windows, the config file will be looked for in:

* `%APPDATA%\alacritty\alacritty.toml`

## Contributing

A guideline about contributing to Alacritty can be found in the
[`CONTRIBUTING.md`](CONTRIBUTING.md) file.

## FAQ

**_Is it really the fastest terminal emulator?_**

Benchmarking terminal emulators is complicated. Alacritty uses
[vtebench](https://github.com/alacritty/vtebench) to quantify terminal emulator
throughput and manages to consistently score better than the competition using
it. If you have found an example where this is not the case, please report a
bug.

Other aspects like latency or framerate and frame consistency are more difficult
to quantify. Some terminal emulators also intentionally slow down to save
resources, which might be preferred by some users.

If you have doubts about Alacritty's performance or usability, the best way to
quantify terminal emulators is always to test them with **your** specific
usecases.

**_Why isn't feature X implemented?_**

Alacritty has many great features, but not every feature from every other
terminal. This could be for a number of reasons, but sometimes it's just not a
good fit for Alacritty. This means you won't find things like tabs or splits
(which are best left to a window manager or [terminal multiplexer][tmux]) nor
niceties like a GUI config editor.

[tmux]: https://github.com/tmux/tmux

## License

Alacritty is released under the [Apache License, Version 2.0].

[Apache License, Version 2.0]: https://github.com/alacritty/alacritty/blob/master/LICENSE-APACHE
