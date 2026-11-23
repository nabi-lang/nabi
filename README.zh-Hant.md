<p align="center">
  <img src="assets/logo.svg" alt="Nabi" width="96">
</p>

# Nabi

[![CI](https://github.com/nabi-lang/nabi/actions/workflows/ci.yml/badge.svg)](https://github.com/nabi-lang/nabi/actions/workflows/ci.yml) [![release](https://img.shields.io/github/v/release/nabi-lang/nabi?include_prereleases&sort=semver)](https://github.com/nabi-lang/nabi/releases) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#授權條款)

*規則清晰，如蝶般自由。*

[English](README.md) · [한국어](README.ko.md) · [简体中文](README.zh-Hans.md) · 繁體中文

Nabi 語言的實作：編譯器 `nabic`、套件與建置工具 `nabi`、語言伺服器 `nabils`、
執行環境 `libnabir.a`，以及標準函式庫（`library/{core,std,bare}`）。

- 網站：https://nabi-lang.org/zh-hant/
- 文件（指南、參考、標準函式庫 API）：https://docs.nabi-lang.org
- 語言規格：https://github.com/nabi-lang/spec

## 安裝

macOS、Linux 與 FreeBSD：

```sh
curl -fsSL https://nabi.sh | sh
```

Windows（x64 或 arm64），在 PowerShell 中：

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://nabi.sh/install.ps1 | iex"
```

安裝程式會像 rustup 一樣把 `~/.nabi/bin` 加入 `PATH`：`install.sh` 寫入
`~/.nabi/env`，並在 `~/.profile`、`~/.bashrc`、`~/.bash_profile` 與
`~/.zshenv`（fish 為 `conf.d/nabi.fish`）中載入它；`install.ps1` 設定使用者
`PATH`。它也會安裝 `nabi` 的 shell 自動完成（`nabi completions <shell>` 會輸出
指令稿）。安裝後請開啟新的 shell。若不想變更 shell 設定檔，可傳入
`--no-modify-path`（`curl -fsSL https://nabi.sh | sh -s -- --no-modify-path`）或
`-NoModifyPath`，或設定 `NABI_NO_MODIFY_PATH=1`。
工具鏈內含官方套件，因此使用這些套件的程式碼不需網路即可建置。若不需要，可傳入
`--no-packages`（`-NoPackages`）或設定 `NABI_NO_PACKAGES=1`（之後也能用
`nabi component remove packages` 移除）。

Nix：

```sh
nix profile install github:nabi-lang/nabi
```

[下載頁面](https://nabi-lang.org/zh-hant/download.html)提供發行封存檔、檢查碼與
跨平台編譯目標。每個版本的 `SHA256SUMS` 都以 minisign 簽署（`keys/release.pub`，
金鑰 `B33E6FE61BE5314C`）；`nabi toolchain update` 不會安裝任何未通過驗證的內容，
當 `PATH` 中有 `minisign` 時，安裝程式也會驗證簽章（docs/releasing.md 的
「Signing」一節）。除此之外不需要其他東西：工具鏈有自己的程式碼產生器與連結器，
並附上所有目標的執行環境，因此在任何主機上都能用 `--target <triple>` 跨平台編譯
（Apple 目標只能在 macOS 上建置；Android 目標需要 NDK）。

## 快速開始

```sh
nabi new hello        # hello/{src/main.nabi, nabi.nabion, .gitignore} + git init
cd hello
nabi run              # Hello, world!
nabi repl .           # 載入套件繫結的 REPL 工作階段
nabi test
nabi jupyter install  # 供 Jupyter / VS Code 使用的 Nabi 核心
```

## 從原始碼建置

建置需要 rustup（含 `rust-toolchain.toml` 中固定的 Rust 工具鏈），以及 Rust
本身使用的 C 編譯器與連結器；下方的執行環境步驟還需要 `curl`、`python3` 與支援
xz 的 `tar`（它會為 `llvm-ar` 加入 rustup 的 `llvm-tools` 元件）。

```sh
cargo build --release                                     # target/release/{nabi,nabic,nabils} 與主機執行環境 libnabir.a
targets/build.sh "$(rustc -vV | sed -n 's/^host: //p')"   # 主機的 C 函式庫（Linux），讓 nabic 能連結程式
```

`cargo build` 會產生執行檔與主機執行環境（`target/<profile>/libnabir.a`）。
工具鏈隨附 C 函式庫的目標（Linux、wasm32、FreeBSD）以及所有非主機目標，都需要
`target/lib/nabi/targets/<triple>/` 下的檔案，由 `targets/build.sh <triple>`
建置；缺少時 `nabic` 會回報 `no runtime for <triple>`。macOS 與 Windows 主機
不需要其他準備。若要跨平台編譯到其他隨附的目標，請建置該 triple（未指定 triple
則建置主機能建置的所有目標）。

`nabic` 自行產生程式碼（`compiler/nabic_codegen_native`），並以自己的連結器
（`link/nabild`）連結所有目標；可用 `NABI_CC` 指定改用的 C 編譯器驅動程式。
`nabi bindgen` 以編譯器自己的 C 前端（`cc/`）讀取 C 標頭檔，不需要 C 編譯器或
libclang。

檢查：

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --no-fail-fast
./target/debug/nabi fmt --check library
```

`NABI_TEST_FILTER=a,b cargo test -p nabic --test main integration::` 只執行名稱
包含 `a` 或 `b` 的語料案例；空白或未設定時執行所有案例。

封裝（`scripts/dist/*.sh`）、跨平台目標與 wasm 語料，以及 CI 分層說明於
[docs/ci.md](docs/ci.md)。

## 授權條款

`MIT OR Apache-2.0`（[LICENSE-MIT](LICENSE-MIT)、[LICENSE-APACHE](LICENSE-APACHE)）。
規格儲存庫採用 CC-BY-4.0。

nabic 內含依各自授權條款的第三方檔案，列於下方。發行封存檔另含跨平台目標所需的
C 執行環境元件（musl、glibc crt 目的檔、wasi-libc），以及連結進執行檔與執行環境
的 Rust 標準函式庫；封存檔中的所有內容列於
[THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES)，授權全文見
[licenses/](licenses/)。授權內容以英文原文為準。

- `cc/nabicc_pp/include/` 中的內建函式標頭檔（`immintrin.h`、`arm_neon.h`、
  `intrin.h` 等）是以 `cc/nabicc_pp/intrin.py` 精簡為宣告的 LLVM clang 標頭檔：
  `Apache-2.0 WITH LLVM-exception`
  （[LICENSE-LLVM.txt](cc/nabicc_pp/include/LICENSE-LLVM.txt)；`arm_neon.h` 與
  `arm_fp16.h` 帶有 clang 產生時附加的 MIT 聲明）；
- `link/nabild/windows/` 中的 Windows 模組定義來自 mingw-w64
  （[COPYING.mingw-w64](link/nabild/windows/COPYING.mingw-w64)）。
