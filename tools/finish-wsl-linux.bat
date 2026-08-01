@echo off
echo === Beautiful: finish WSL + Linux build ===
echo.
echo 1) Enable Virtualization in BIOS if needed
echo 2) Run as Administrator once:
echo    wsl --install -d Ubuntu-24.04
echo 3) Reboot, open Ubuntu, set username
echo 4) Then run:
echo    wsl -e bash /mnt/c/modding/beautiful/tools/build-linux.sh
echo    powershell -ExecutionPolicy Bypass -File C:\modding\beautiful\tools\pack-alpha.ps1
echo.
pause
