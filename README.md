# ferrolite
Fast, open-source RAW photo editor and library manager, built in Rust with a GPU pipeline.

## Download & Install

Installers are published on the [Releases](https://github.com/FPGSchiba/ferrolite/releases) page:

- **Windows:** `FerroLite_<version>_x64-setup.exe` (NSIS installer)
- **macOS (Apple Silicon):** `FerroLite_<version>_aarch64.dmg`
- **macOS (Intel):** `FerroLite_<version>_x64.dmg`

Builds are currently **unsigned**, so the OS shows a one-time warning:

- **Windows:** SmartScreen "Windows protected your PC" → **More info** → **Run anyway**.
- **macOS:** "unidentified developer" → right-click the app → **Open** (or run
  `xattr -dr com.apple.quarantine /Applications/FerroLite.app`).

To cut a release, push a tag: `git tag v0.1.0 && git push origin v0.1.0`.
