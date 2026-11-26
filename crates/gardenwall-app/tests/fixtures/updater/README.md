# Updater signature fixtures

A throwaway key made only for tests (`tauri signer generate`, no password). Its private half was never kept in the repository; nothing outside these tests trusts it.

- `test-key.pub`: the public key.
- `installer.bin`: stand-in bytes for an installer.
- `installer.bin.sig`: `tauri signer sign --app-version 1.0.1 installer.bin` (the trusted comment names version 1.0.1).
