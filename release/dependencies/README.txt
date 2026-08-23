OpenLEAudio dependency cache
============================

This directory may remain empty. INSTALL dependencies.bat downloads missing
official Microsoft installers here when the user approves the download:

- windowsdesktop-runtime-8-x64.exe
- windows-app-runtime-1.8-x64.exe
- vc-redist-2015-2022-x64.exe
- VBCABLE_Driver_Pack45.zip

The Visual C++ 2015-2022 runtime is easy to overlook: neither Microsoft
installer above brings it, and without it the application exits immediately
after launch with no window and no error, which looks like a corrupt download
rather than a missing dependency.

For an offline package, add the installers before creating the ZIP. They are not
required in the normal small release and are intentionally excluded from Git.
