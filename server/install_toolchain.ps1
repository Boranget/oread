$env:RUSTUP_HOME = "D:\tools\rust\.rustup"
$env:CARGO_HOME = "D:\tools\rust\.cargo"
$env:RUSTUP_DIST_SERVER = "https://mirrors.ustc.edu.cn/rust-static"
$env:PATH = "D:\tools\rust\.cargo\bin;D:\tools\MinGW\mingw64\bin;" + $env:PATH
"=== installing gnu-host toolchain (ustc mirror) ===" *>> d:\projects\read\server\install.log
rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal *>> d:\projects\read\server\install.log
rustup default stable-x86_64-pc-windows-gnu *>> d:\projects\read\server\install.log
rustc -vV *>> d:\projects\read\server\install.log
Start-Process -FilePath "powershell.exe" -ArgumentList "-ExecutionPolicy Bypass -File d:\projects\read\server\run_build.ps1" -WindowStyle Hidden
