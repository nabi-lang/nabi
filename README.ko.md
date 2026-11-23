<p align="center">
  <img src="assets/logo.svg" alt="Nabi" width="96">
</p>

# Nabi

[![CI](https://github.com/nabi-lang/nabi/actions/workflows/ci.yml/badge.svg)](https://github.com/nabi-lang/nabi/actions/workflows/ci.yml) [![release](https://img.shields.io/github/v/release/nabi-lang/nabi?include_prereleases&sort=semver)](https://github.com/nabi-lang/nabi/releases) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#라이선스)

*규칙은 분명하게, 나비처럼 자유롭게.*

[English](README.md) · 한국어 · [简体中文](README.zh-Hans.md) · [繁體中文](README.zh-Hant.md)

Nabi 언어의 구현입니다. 컴파일러 `nabic`, 패키지·빌드 도구 `nabi`, 언어 서버
`nabils`, 런타임 `libnabir.a`, 표준 라이브러리(`library/{core,std,bare}`)가
들어 있습니다.

- 사이트: https://nabi-lang.org/ko/
- 문서(가이드, 레퍼런스, 표준 라이브러리 API): https://docs.nabi-lang.org
- 언어 명세: https://github.com/nabi-lang/spec

## 설치

macOS, Linux, FreeBSD:

```sh
curl -fsSL https://nabi.sh | sh
```

Windows(x64, arm64)에서는 PowerShell로:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://nabi.sh/install.ps1 | iex"
```

설치 스크립트는 rustup과 같은 방식으로 `~/.nabi/bin`을 `PATH`에 넣습니다.
`install.sh`는 `~/.nabi/env`를 만들고 `~/.profile`, `~/.bashrc`,
`~/.bash_profile`, `~/.zshenv`(fish는 `conf.d/nabi.fish`)에서 불러오게 하며,
`install.ps1`은 사용자 `PATH`를 설정합니다. `nabi`의 셸 자동 완성도 함께
설치합니다(`nabi completions <shell>`이 스크립트를 출력합니다). 설치 후 새 셸을
여세요. 셸 설정 파일을 건드리지 않으려면 `--no-modify-path`
(`curl -fsSL https://nabi.sh | sh -s -- --no-modify-path`)나 `-NoModifyPath`를
넘기거나 `NABI_NO_MODIFY_PATH=1`을 설정하세요.
툴체인에는 공식 패키지가 들어 있어서, 이를 쓰는 코드는 네트워크 없이 빌드됩니다.
빼려면 `--no-packages`(`-NoPackages`)를 넘기거나 `NABI_NO_PACKAGES=1`을 설정하세요
(나중에 `nabi component remove packages`로도 뺄 수 있습니다).

Nix:

```sh
nix profile install github:nabi-lang/nabi
```

[다운로드 페이지](https://nabi-lang.org/ko/download.html)에 릴리스 아카이브와
체크섬, 크로스 컴파일 대상이 있습니다. 각 릴리스의 `SHA256SUMS`는 minisign으로
서명됩니다(`keys/release.pub`, 키 `B33E6FE61BE5314C`). `nabi toolchain update`는
검증되지 않는 것은 설치하지 않으며, 설치 스크립트도 `PATH`에 `minisign`이 있으면
서명을 확인합니다(docs/releasing.md의 "Signing"). 그 밖에 필요한 것은 없습니다.
툴체인에는 자체 코드 생성기와 링커가 있고 모든 대상의 런타임이 들어 있어서,
어떤 호스트에서든 `--target <triple>`로 크로스 컴파일할 수 있습니다(Apple 대상은
macOS에서만, Android 대상에는 NDK가 필요).

## 빠른 시작

```sh
nabi new hello        # hello/{src/main.nabi, nabi.nabion, .gitignore} + git init
cd hello
nabi run              # Hello, world!
nabi repl .           # 패키지의 바인딩을 불러온 REPL
nabi test
nabi jupyter install  # Jupyter / VS Code용 Nabi 커널
```

## 소스에서 빌드하기

빌드에는 `rust-toolchain.toml`에 고정된 Rust 툴체인이 설치된 rustup과, Rust가
쓰는 C 컴파일러와 링커가 필요합니다. 아래 런타임 단계에는 `curl`, `python3`,
xz를 지원하는 `tar`도 필요합니다(`llvm-ar`를 위해 rustup의 `llvm-tools`
컴포넌트를 추가합니다).

```sh
cargo build --release                                     # target/release/{nabi,nabic,nabils}와 호스트 런타임 libnabir.a
targets/build.sh "$(rustc -vV | sed -n 's/^host: //p')"   # 호스트의 C 라이브러리(Linux). nabic이 프로그램을 링크하는 데 씁니다
```

`cargo build`는 바이너리와 호스트 런타임(`target/<profile>/libnabir.a`)을
만듭니다. 툴체인이 C 라이브러리를 함께 배포하는 대상(Linux, wasm32, FreeBSD)과
호스트가 아닌 모든 대상은 `target/lib/nabi/targets/<triple>/` 아래에 파일이
있어야 하며, `targets/build.sh <triple>`이 이를 빌드합니다. 없으면 `nabic`이
`no runtime for <triple>`을 보고합니다. macOS와 Windows 호스트는 더 필요한 것이
없습니다. 다른 대상으로 크로스 컴파일하려면 그 triple을 빌드하세요(triple을
주지 않으면 호스트가 빌드할 수 있는 모든 대상을 빌드합니다).

`nabic`은 코드를 직접 생성하고(`compiler/nabic_codegen_native`) 모든 대상을 자체
링커(`link/nabild`)로 링크합니다. 대신 쓸 C 컴파일러 드라이버는 `NABI_CC`로
지정합니다. `nabi bindgen`은 컴파일러 자체의 C 프런트엔드(`cc/`)로 C 헤더를
읽으므로 C 컴파일러나 libclang이 필요 없습니다.

검사:

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --no-fail-fast
./target/debug/nabi fmt --check library
```

`NABI_TEST_FILTER=a,b cargo test -p nabic --test main integration::`는 이름에
`a`나 `b`가 들어간 코퍼스 케이스만 실행합니다. 비어 있거나 설정하지 않으면 모든
케이스를 실행합니다.

패키징(`scripts/dist/*.sh`), 크로스 대상과 wasm 코퍼스, CI 단계는
[docs/ci.md](docs/ci.md)에 설명되어 있습니다.

## 라이선스

`MIT OR Apache-2.0`([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)).
명세 저장소는 CC-BY-4.0입니다.

nabic에는 각자의 라이선스를 따르는 서드파티 파일이 들어 있으며, 아래에
적었습니다. 릴리스 아카이브에는 크로스 대상용 C 런타임 조각(musl, glibc crt
오브젝트, wasi-libc)과, 바이너리와 런타임에 링크된 Rust 표준 라이브러리도
들어 있습니다. 아카이브에 든 모든 것은
[THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES)에, 라이선스 원문은
[licenses/](licenses/)에 있습니다. 라이선스 내용은 영어 원문이 기준입니다.

- `cc/nabicc_pp/include/`의 intrinsic 헤더(`immintrin.h`, `arm_neon.h`,
  `intrin.h` 등)는 LLVM clang 헤더를 `cc/nabicc_pp/intrin.py`로 선언만 남긴
  것입니다: `Apache-2.0 WITH LLVM-exception`
  ([LICENSE-LLVM.txt](cc/nabicc_pp/include/LICENSE-LLVM.txt). `arm_neon.h`와
  `arm_fp16.h`에는 clang이 생성할 때 붙이는 MIT 고지가 있습니다)
- `link/nabild/windows/`의 Windows 모듈 정의는 mingw-w64의 것입니다
  ([COPYING.mingw-w64](link/nabild/windows/COPYING.mingw-w64)).
