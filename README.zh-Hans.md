<p align="center">
  <img src="assets/logo.svg" alt="Nabi" width="96">
</p>

# Nabi

[![CI](https://github.com/nabi-lang/nabi/actions/workflows/ci.yml/badge.svg)](https://github.com/nabi-lang/nabi/actions/workflows/ci.yml) [![release](https://img.shields.io/github/v/release/nabi-lang/nabi?include_prereleases&sort=semver)](https://github.com/nabi-lang/nabi/releases) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#许可证)

*规则清晰，如蝶般自由。*

[English](README.md) · [한국어](README.ko.md) · 简体中文 · [繁體中文](README.zh-Hant.md)

Nabi 语言的实现：编译器 `nabic`、包管理与构建工具 `nabi`、语言服务器
`nabils`、运行时 `libnabir.a`，以及标准库（`library/{core,std,bare}`）。

- 网站：https://nabi-lang.org/zh-hans/
- 文档（指南、参考、标准库 API）：https://docs.nabi-lang.org
- 语言规范：https://github.com/nabi-lang/spec

## 安装

macOS、Linux 和 FreeBSD：

```sh
curl -fsSL https://nabi.sh | sh
```

Windows（x64 或 arm64），在 PowerShell 中：

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://nabi.sh/install.ps1 | iex"
```

安装脚本像 rustup 一样把 `~/.nabi/bin` 加入 `PATH`：`install.sh` 写入
`~/.nabi/env`，并在 `~/.profile`、`~/.bashrc`、`~/.bash_profile` 和
`~/.zshenv`（fish 为 `conf.d/nabi.fish`）中加载它；`install.ps1` 设置用户
`PATH`。它还会安装 `nabi` 的 shell 补全（`nabi completions <shell>` 输出脚本）。
安装后请打开新的 shell。若不想改动 shell 配置文件，可传入 `--no-modify-path`
（`curl -fsSL https://nabi.sh | sh -s -- --no-modify-path`）或 `-NoModifyPath`，
或设置 `NABI_NO_MODIFY_PATH=1`。
工具链自带官方包，因此使用它们的代码无需联网即可构建。若不需要，可传入
`--no-packages`（`-NoPackages`）或设置 `NABI_NO_PACKAGES=1`（之后也可用
`nabi component remove packages` 移除）。

Nix：

```sh
nix profile install github:nabi-lang/nabi
```

[下载页面](https://nabi-lang.org/zh-hans/download.html)提供发布归档、校验和以及
交叉编译目标。每个版本的 `SHA256SUMS` 都用 minisign 签名（`keys/release.pub`，
密钥 `B33E6FE61BE5314C`）；`nabi toolchain update` 不会安装任何未通过验证的内容，
当 `PATH` 中有 `minisign` 时，安装脚本也会校验签名（docs/releasing.md 的
“Signing”一节）。除此之外无需其他依赖：工具链自带代码生成器和链接器，并附带
所有目标的运行时，因此在任何主机上都能用 `--target <triple>` 交叉编译。

## 快速开始

```sh
nabi new hello        # hello/{src/main.nabi, nabi.nabion, .gitignore} + git init
cd hello
nabi run              # Hello, world!
nabi repl .           # 载入包中绑定的 REPL 会话
nabi test
nabi jupyter install  # 用于 Jupyter / VS Code 的 Nabi 内核
```

## 从源码构建

构建需要 rustup（含 `rust-toolchain.toml` 中固定的 Rust 工具链），以及 Rust
本身使用的 C 编译器和链接器；下面的运行时步骤还需要 `curl`、`python3` 和支持
xz 的 `tar`（它会为 `llvm-ar` 添加 rustup 的 `llvm-tools` 组件）。

```sh
cargo build --release                                     # target/release/{nabi,nabic,nabils} 与主机运行时 libnabir.a
targets/build.sh "$(rustc -vV | sed -n 's/^host: //p')"   # 主机的 C 库（Linux），供 nabic 链接程序
```

`cargo build` 生成二进制文件和主机运行时（`target/<profile>/libnabir.a`）。
工具链随附 C 库的目标（Linux、wasm32、FreeBSD）以及所有非主机目标，都需要
`target/lib/nabi/targets/<triple>/` 下的文件，由 `targets/build.sh <triple>`
构建；缺少时 `nabic` 会报告 `no runtime for <triple>`。macOS 和 Windows 主机
无需更多准备。要交叉编译到其他随附的目标，请构建对应的 triple（不指定 triple
则构建主机能构建的全部目标）。

`nabic` 自行生成代码（`compiler/nabic_codegen_native`），并用自带的链接器
（`link/nabild`）链接所有目标；可用 `NABI_CC` 指定改用的 C 编译器驱动。
`nabi bindgen` 使用编译器自带的 C 前端（`cc/`）读取 C 头文件，不需要 C 编译器
或 libclang。

检查：

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --no-fail-fast
./target/debug/nabi fmt --check library
```

`NABI_TEST_FILTER=a,b cargo test -p nabic --test main integration::` 只运行名称
包含 `a` 或 `b` 的语料用例；为空或未设置时运行全部用例。

打包（`dist-*.sh`）、交叉目标与 wasm 语料以及 CI 分层见
[docs/ci.md](docs/ci.md)。

## 许可证

`MIT OR Apache-2.0`（[LICENSE-MIT](LICENSE-MIT)、[LICENSE-APACHE](LICENSE-APACHE)）。
规范仓库采用 CC-BY-4.0。

nabic 内含遵循各自许可证的第三方文件，列于下方。发布归档还包含交叉目标所需的
C 运行时部件（musl、glibc crt 目标文件、wasi-libc），以及链接进二进制文件和
运行时的 Rust 标准库；归档中的全部内容列于
[THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES)，许可证全文见
[licenses/](licenses/)。许可证内容以英文原文为准。

- `cc/nabicc_pp/include/` 中的内建函数头文件（`immintrin.h`、`arm_neon.h`、
  `intrin.h` 等）是由 `cc/nabicc_pp/intrin.py` 精简为声明的 LLVM clang 头文件：
  `Apache-2.0 WITH LLVM-exception`
  （[LICENSE-LLVM.txt](cc/nabicc_pp/include/LICENSE-LLVM.txt)；`arm_neon.h` 和
  `arm_fp16.h` 带有 clang 生成时附加的 MIT 声明）；
- `link/nabild/windows/` 中的 Windows 模块定义来自 mingw-w64
  （[COPYING.mingw-w64](link/nabild/windows/COPYING.mingw-w64)）。
