# CC Proxy

## 1. 安装 Rust

本项目使用 Rust 编写。如果你的系统尚未安装 Rust 环境，请通过以下方式安装：

- **Linux / macOS**:
  打开终端并运行以下命令：
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **Windows**:
  请前往 [Rust 官网的安装页面](https://www.rust-lang.org/tools/install) 下载并运行 `rustup-init.exe`。

> 注意：安装完成后，建议重启终端或按照安装程序的提示应用环境变量（例如运行 `source $HOME/.cargo/env`）。

## 2. 编译项目

在项目根目录（即含有 `Cargo.toml` 的目录）下打开终端，运行以下命令即可编译整个项目。推荐使用 `--release` 参数来进行优化编译，以获得更好的运行性能：

```bash
cargo build --release
```

编译完成后，生成的可执行文件将位于 release 目录下。

## 3. 运行项目

你可以直接通过 `cargo` 命令来运行项目：

```bash
cargo run --release
```

或者直接执行编译产生出来的二进制文件（以默认环境为例）：

- **Linux / macOS**:
  ```bash
  ./target/release/cc-proxy
  ```
- **Windows**:
  ```powershell
  .\target\release\cc-proxy.exe
  ```
```

