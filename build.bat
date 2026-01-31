@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
echo Running cargo...
cargo run 2>&1
echo Exit code: %errorlevel%
