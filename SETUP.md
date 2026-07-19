# LimeLoader — full setup & build guide

This guide covers building LimeLoader from source and publishing it under the
**`Lime-Loader`** GitHub org. LimeLoader is an Android/IL2CPP Unity mod loader (based on
LemonLoader 0.6.x + MelonLoader 0.7.x fixes) that supports the newest Unity versions
(Unity 6 / `6000.x`).

Everything below was verified on Linux (Arch) with the toolchain versions listed. Windows
notes are called out where they differ.

---

## 0. GitHub org layout to create under `github.com/Lime-Loader`

LimeLoader references these repos. Create them in the org (fork the upstreams) before you
publish, or the submodule clone in step 2 will fail:

| Repo | Where it's used | Source to fork |
| - | - | - |
| `Lime-Loader/LimeLoader` | the loader itself (this repo) | your LimeLoader tree |
| `Lime-Loader/LimeLoaderInstaller` | the installer | `LemonLoader/MelonLoaderInstaller` |
| `Lime-Loader/MonoMod` | submodule `External/MonoMod` | `LemonLoader/MonoMod` |
| `Lime-Loader/HarmonyX` | submodule `External/HarmonyX` | `LemonLoader/HarmonyX` |
| `Lime-Loader/Il2CppInterop` | submodule `External/Il2CppInterop` | `LemonLoader/Il2CppInterop` |
| `Lime-Loader/JNISharp` | submodule `External/JNISharp` | `LemonLoader/JNISharp` |

Kept pointing at upstream (not forked): `External/Cpp2IL` → `SamboyCoding/Cpp2IL`, and the
Rust crate patches (`TrevTV/Ferrex`, `RinLovesYou/*`). Optional China mirror:
`Lime-Loader/LimeLoader.UnityDependencies.China` (the installer falls back to it in the CN
region; the primary Unity-deps host stays `LavaGang/MelonLoader.UnityDependencies`).

Submodule URLs are already set to the org in `.gitmodules`.

---

## 1. Toolchains

Install user-locally (no root needed). Search before installing — you may already have some.

### .NET SDKs — 8 **and** 9 (both required)
`net35`/`net8` targets build on either, but `Cpp2IL.Core` uses C# 13, which needs the .NET 9 SDK.
```bash
curl -fsSL https://dot.net/v1/dotnet-install.sh -o dotnet-install.sh && chmod +x dotnet-install.sh
./dotnet-install.sh --channel 8.0 --install-dir "$HOME/.dotnet"
./dotnet-install.sh --channel 9.0 --install-dir "$HOME/.dotnet"
export PATH="$HOME/.dotnet:$PATH" DOTNET_ROOT="$HOME/.dotnet"
```

### Rust + Android target + cargo-ndk (for the native bootstrap)
```bash
curl -fsSL https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
source "$HOME/.cargo/env"
rustup target add aarch64-linux-android
cargo install cargo-ndk
```

### Android NDK r26d
```bash
mkdir -p "$HOME/ndk" && cd "$HOME/ndk"
curl -fL -O https://dl.google.com/android/repository/android-ndk-r26d-linux.zip
unzip -q android-ndk-r26d-linux.zip
export ANDROID_NDK_HOME="$HOME/ndk/android-ndk-r26d"
```
(On Windows use the `.zip` for windows and set `ANDROID_NDK_HOME` accordingly. `sdkmanager
"ndk;26.3.11579264"` also works if your SDK dir is writable.)

---

## 2. Clone with submodules
```bash
git clone --recurse-submodules https://github.com/Lime-Loader/LimeLoader.git
cd LimeLoader
# MonoMod has a nested submodule (iced):
git submodule update --init --recursive
```

---

## 3. Build the managed loader (.NET)

Build **MonoMod first** — the core references its compiled artifacts.
```bash
export PATH="$HOME/.dotnet:$PATH" DOTNET_ROOT="$HOME/.dotnet"
SLN="$(pwd)/"

# 3a. MonoMod (net8 + net35 artifacts consumed by the core)
dotnet build External/MonoMod/src/MonoMod.RuntimeDetour/MonoMod.RuntimeDetour.csproj -c Debug
dotnet build External/MonoMod/src/MonoMod.Patcher/MonoMod.Patcher.csproj -c Debug

# 3b. Core loader, entry host, support modules, il2cpp assembly generator
dotnet build MelonLoader/MelonLoader.csproj                 -c Debug -f net8  -p:SolutionDir="$SLN"
dotnet build MelonLoader/MelonLoader.csproj                 -c Debug -f net35 -p:SolutionDir="$SLN"
dotnet build MelonLoader.NativeHost/MelonLoader.NativeHost.csproj -c Debug   -p:SolutionDir="$SLN"
dotnet build Dependencies/SupportModules/Il2Cpp/Il2Cpp.csproj    -c Debug    -p:SolutionDir="$SLN"
dotnet build Dependencies/SupportModules/Mono/Mono.csproj        -c Debug    -p:SolutionDir="$SLN"
dotnet build Dependencies/Il2CppAssemblyGenerator/Il2CppAssemblyGenerator.csproj -c Debug -p:SolutionDir="$SLN"

# 3c. Unity 6 helper assemblies (asset bundles + image conversion)
dotnet build UnityUtilities/UnityEngine.Il2CppAssetBundleManager/UnityEngine.Il2CppAssetBundleManager.csproj     -c Debug -p:SolutionDir="$SLN"
dotnet build UnityUtilities/UnityEngine.Il2CppImageConversionManager/UnityEngine.Il2CppImageConversionManager.csproj -c Debug -p:SolutionDir="$SLN"
```
Managed output lands in `Output/Debug/MelonLoader/`. Use `-c Release` for release builds.

> `-p:SolutionDir="$SLN"` is required when building individual projects, otherwise output
> paths resolve to `/Output` and copies fail.
>
> **Preload** (`Dependencies/SupportModules/Preload`, net20) does **not** build with the
> .NET Core CLI on Linux — its `.resx` needs `ResGen.exe`. Build it on Windows or with
> Mono's `resgen`. It only runs on Mono runtimes older than .NET 3.0, so it is not needed
> for the Android/IL2CPP target.

---

## 4. Build the native Android bootstrap (Rust)
```bash
source "$HOME/.cargo/env"
export ANDROID_NDK_HOME="$HOME/ndk/android-ndk-r26d"
cargo ndk -t arm64-v8a -o ./jniLibs build          # or build-debug.bat on Windows
# Release: cargo ndk -t arm64-v8a -o ./jniLibs build --release
```
Produces `jniLibs/arm64-v8a/libBootstrap.so` and `libmain.so` (ARM64, Android 21+).

> If you regenerate `Cargo.lock` from scratch, run `cargo update` once so `dlopen2_derive`
> resolves to ≥ 0.4.3 (0.4.0 fails to compile `hostfxr-sys`).

---

## 5. Assemble the loader payload (`melon_data`)

The installer downloads a release asset whose name starts with **`melon_data`** from
`Lime-Loader/LimeLoader`'s latest release. That asset is a zip laid out as:
```
melon_data.zip
├── MelonLoader/            # Output/Debug(Release)/MelonLoader/  (managed loader + Dependencies)
├── dotnet/                 # the .NET runtime for Android (from BaseLibs/dotnet_fixed_gc etc.)
└── native/                 # jniLibs/arm64-v8a/*.so  + libunity/openssl .so as needed
```
Zip these up and attach the zip to a GitHub release on `Lime-Loader/LimeLoader` as
`melon_data.zip`. (Keep the `melon_data` prefix — the installer matches on it. The internal
folder name `MelonLoader` must also stay: the Rust bootstrap and installer both hard-code it.)

---

## 6. Build & run the installer

The installer (`../LemonLoaderInstaller-main`, publish as `Lime-Loader/LimeLoaderInstaller`)
is a .NET MAUI app (Android + Windows). It is already rebranded to LimeLoader and points at
`Lime-Loader/LimeLoader` releases.
```bash
dotnet workload install maui android
# Provide a signing keystore (the app id is com.limeloader.installer):
keytool -genkey -v -keystore App/limeinstaller.keystore -alias lime \
        -keyalg RSA -keysize 2048 -validity 10000
export LIME_KEYSTORE_PASS=yourpassword
dotnet build App/MelonLoader.Installer.App.csproj -c Release -f net8.0-android
```
The APK lands under `App/bin/Release/net8.0-android/`. Install it on the device, point it at
the target IL2CPP app, and it patches the APK using the `melon_data` payload from step 5.

---

## 7. What builds where — summary

| Component | Toolchain | Status |
| - | - | - |
| MonoMod (fork) | .NET 8/9 | ✅ builds |
| Core `MelonLoader` (net8 + net35) | .NET 8/9 | ✅ builds |
| `MelonLoader.NativeHost` | .NET 8 | ✅ builds |
| Support modules (Il2Cpp/Mono) | .NET 8 / .NET 35 | ✅ builds |
| Il2Cpp assembly generator | .NET 9 (C# 13) | ✅ builds |
| Unity 6 helpers (AssetBundle/ImageConversion) | .NET 8 | ✅ builds |
| Rust bootstrap (`libBootstrap.so`, `libmain.so`) | Rust + NDK r26d | ✅ builds (arm64) |
| `Preload` (net20) | Windows / Mono resgen | ⚠️ Windows/Mono only, IL2CPP-irrelevant |

See `LIMELOADER.md` for the exact source changes vs. the LemonLoader base.
