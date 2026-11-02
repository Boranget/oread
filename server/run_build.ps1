$env:RUSTUP_HOME = "D:\tools\rust\.rustup"
$env:CARGO_HOME = "D:\tools\rust\.cargo"
$env:PATH = "D:\tools\rust\.cargo\bin;D:\tools\MinGW\mingw64\bin;" + $env:PATH
Set-Location "d:\projects\read\server"
cargo build --target x86_64-pc-windows-gnu *> "d:\projects\read\server\build.log"
