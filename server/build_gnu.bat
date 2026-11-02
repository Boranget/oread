@echo off
set RUSTUP_HOME=D:\tools\rust\.rustup
set CARGO_HOME=D:\tools\rust\.cargo
set PATH=D:\tools\rust\.cargo\bin;D:\tools\MinGW\mingw64\bin;%PATH%
cd /d d:\projects\read\server
echo === build start %DATE% %TIME% === > build.log
rustup target add x86_64-pc-windows-gnu >> build.log 2>&1
echo >>> cargo build --target x86_64-pc-windows-gnu >> build.log
cargo build --target x86_64-pc-windows-gnu >> build.log 2>&1
echo === build done %DATE% %TIME% === >> build.log
