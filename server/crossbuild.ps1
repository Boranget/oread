Set-Location 'd:\projects\read\server'
$env:RUSTUP_HOME='D:\tools\rust\.rustup'
$env:CARGO_HOME='D:\tools\rust\.cargo'
$env:RUSTUP_DIST_SERVER='https://mirrors.tuna.tsinghua.edu.cn/rustup'
$env:PATH='D:\tools\rust\.cargo\bin;D:\tools\MinGW\mingw64\bin;D:\tools\zig\zig-windows-x86_64-0.13.0;' + $env:PATH
rustup target add aarch64-unknown-linux-musl *>> crossbuild.log
cargo zigbuild --target aarch64-unknown-linux-musl --release *>> crossbuild.log

