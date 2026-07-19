# LimeLoader

LimeLoader is an Android/IL2CPP Unity mod loader derived from **LemonLoader 0.6.x**
(itself a fork of MelonLoader), updated to support the **newest Unity versions**
(Unity 6 / `6000.x`) by pulling in fixes from **MelonLoader 0.7.x (master)**.

It keeps LemonLoader's Android/ARM64 Rust bootstrap and JNI layer intact, and layers
the newer Unity-compatibility toolchain on top.

## What changed vs. the LemonLoader base

### Feature: randomized setup GameObject name
- `Dependencies/SupportModules/Component.cs`
  The persistent GameObject that hosts the loader's `MonoBehaviour` (update loop,
  coroutines, scene events) is now created with a **fresh random name every run**
  (lowercase alphanumeric, 8–16 chars, e.g. `2hub4t843`, `un2jmnajk`) via a new
  `GenerateRandomName()` helper, instead of the default constructor name. A new name
  is generated on each `Create()` (including the recreate-on-destroy path), so it also
  differs if the object is rebuilt mid-session. Nothing looks the object up by name
  (verified: no `GameObject.Find`), so this is safe.

### Rebrand to LimeLoader
- `MelonLoader/Properties/BuildInfo.cs` — product identity is now `LimeLoader` v`0.1.0`.
- `MelonLoader/Core.cs` — `GetVersionString()` now uses `BuildInfo.Name`, so the console
  banner and window title read `LimeLoader vX …`.

> The **namespace stays `MelonLoader`** and the **on-disk folder stays `MelonLoader`** on
> purpose: mods reference the `MelonLoader.*` API, and both the Rust bootstrap
> (`Rust/Bootstrap/src/melonenv/paths.rs` → `BASE_DIR.join("MelonLoader")`) and the
> installer (`LemonLoaderInstaller-main`, `assets/MelonLoader`) hard-code that path.
> Renaming it would require coordinated changes across the bootstrap and installer.

### Newest-Unity fixes ported from MelonLoader master
- `MelonLoader/Resources/classdata.tpk` — replaced the 179 KB type database with
  master's **1.37 MB** database, which contains the serialized-type info for the newest
  Unity versions (needed to read `globalgamemanagers` / `PlayerSettings` on Unity 6).
- `MelonLoader/MelonLoader.csproj` — `AssetsTools.NET` `3.0.0` → **`3.0.4`** to match the
  newer `classdata.tpk` format.
- `MelonLoader/InternalUtils/UnityInformationHandler.cs` — cherry-picked master's
  resilience improvements that only use APIs Lime already has: an outer `try/catch`
  around game-info reading, and a `"Possible out-dated classdata.tpk"` warning when
  `PlayerSettings` can't be found (the Android `APKAssetManager` path and
  `AssetRipper.VersionUtilities` version type were **kept**, not replaced).
- `Dependencies/Il2CppAssemblyGenerator/Packages/Cpp2IL.cs` — forward-ported the
  in-process Cpp2IL driver to the current Cpp2IL API:
  `Cpp2IlRuntimeArgs.OutputFormat` (single) → `OutputFormats` (collection), and
  `WasmFile.RemappedDynCallFunctions` → `LibCpp2IlBinaryRegistry.WasmRemappedDynCallFunctions`.
  This is what lets the newest Cpp2IL (newest IL2CPP metadata support) drive assembly
  generation.
- `Dependencies/Il2CppAssemblyGenerator/Packages/Il2CppInterop.cs` — updated
  `InteropResolver.Resolve` to AsmResolver 6.0's `INetModuleResolver.Resolve(string, ModuleDefinition)`
  signature.

The newest-Unity IL2CPP support itself comes from the External submodules, which are
current (the `LemonLoader/Il2CppInterop` fork HEAD is dated 2026-07-08, and Cpp2IL is at
HEAD): together they read the newest IL2CPP `global-metadata.dat` versions.

## Build environment accommodations (local toolchain)
These make the tree build with the SDKs installed here; they do not change loader behaviour:
- `External/MonoMod/global.json` — `rollForward` set to `latestMajor` (pin was `8.0.301`).
- `External/Cpp2IL/**` — `net10.0`/`net9.0` removed from multi-target lists so the highest
  target is `net8.0` (the assembly generator only consumes `net8.0`). Cpp2IL.Core keeps
  `LangVersion 13`, which needs the .NET 9 SDK's compiler.

## Build status (verified here, Linux, .NET 8.0.423 + .NET 9.0.316 SDKs)

Managed loader — **builds clean (0 errors)**:
- `MelonLoader/MelonLoader.csproj` — `net8` and `net35`
- `Dependencies/SupportModules/Il2Cpp/Il2Cpp.csproj`
- `Dependencies/SupportModules/Mono/Mono.csproj`
- `Dependencies/Il2CppAssemblyGenerator/Il2CppAssemblyGenerator.csproj`
- `External/MonoMod` (Core, Utils, ILHelpers, RuntimeDetour, Patcher — net8 + net35)

Outputs land in `Output/Debug/MelonLoader/…`.

### How to build the managed side
```bash
export PATH="$HOME/.dotnet:$PATH" DOTNET_ROOT="$HOME/.dotnet"
SLN="$(pwd)/"   # run from the LimeLoader root

# 1) MonoMod (produces artifacts consumed by the core)
dotnet build External/MonoMod/src/MonoMod.RuntimeDetour/MonoMod.RuntimeDetour.csproj -c Debug
dotnet build External/MonoMod/src/MonoMod.Patcher/MonoMod.Patcher.csproj -c Debug

# 2) Core + support modules + assembly generator
dotnet build MelonLoader/MelonLoader.csproj -c Debug -f net8  -p:SolutionDir="$SLN"
dotnet build MelonLoader/MelonLoader.csproj -c Debug -f net35 -p:SolutionDir="$SLN"
dotnet build Dependencies/SupportModules/Il2Cpp/Il2Cpp.csproj      -c Debug -p:SolutionDir="$SLN"
dotnet build Dependencies/SupportModules/Mono/Mono.csproj          -c Debug -p:SolutionDir="$SLN"
dotnet build Dependencies/Il2CppAssemblyGenerator/Il2CppAssemblyGenerator.csproj -c Debug -p:SolutionDir="$SLN"
```

The **Rust/Android bootstrap** now builds too — see "Native bootstrap" below and `SETUP.md`.

### Native bootstrap (Rust → Android arm64)
`cargo ndk -t arm64-v8a -o ./jniLibs build` produces `jniLibs/arm64-v8a/libBootstrap.so`
and `libmain.so`. Getting it to build on the current toolchain required updating the
`.NET`-hosting crates (it did **not** need porting for newest Unity — it just hooks IL2CPP
init):
- Root `Cargo.toml`: removed the `[patch.crates-io] nethost-sys = { git = ... }` patch.
- `Rust/Bootstrap/Cargo.toml`: `netcorehost 0.17.0` →
  `{ version = "0.20", default-features = false, features = ["net8_0"] }`. Disabling default
  features drops the optional `nethost-sys` crate (its Android build script panics trying to
  download nethost); the bootstrap loads hostfxr by explicit path, so nethost isn't needed.
- `cargo update` then resolves `dlopen2_derive` to 0.4.3 (0.4.0 fails to compile `hostfxr-sys`).

### Unity 6 helper assemblies (now applied)
`UnityUtilities/UnityEngine.Il2CppAssetBundleManager` and
`.../UnityEngine.Il2CppImageConversionManager` are ported from MelonLoader master (they add
the Unity 6 `_Injected` icall variants, guarded by `EngineVersion.Major >= 6000`), given
standalone net8 csprojs referencing Lime's core + `External/Il2CppInterop` Runtime +
`Il2CppInterop.ReferenceLibs 1.0.0` + `AssetRipper.VersionUtilities 1.5.0` (to match Lime's
`UnityInformationHandler.EngineVersion` type). The rebuilt DLLs replace the prebuilts in
`BaseLibs/net6/` (originals kept as `*.orig-lemon`). They compile but should still be smoke-
tested against a real Unity 6 game before relying on them.

## Still not buildable here

- **`Preload` support module** (net20): its `.resx` uses `ResGen.exe`, which the .NET Core
  CLI can't run on Linux. Build on Windows / with Mono's `resgen`. It only runs on Mono
  runtimes older than .NET 3.0, so it is not needed for the Android/IL2CPP target.

## Installer

`../LemonLoaderInstaller-main` (publish as `Lime-Loader/LimeLoaderInstaller`) is rebranded to
LimeLoader: display title "LimeLoader Installer", app id `com.limeloader.installer`, signing
keystore/alias `limeinstaller.keystore`/`lime` with `LIME_KEYSTORE_PASS`, user-facing strings,
and release URLs pointing at `Lime-Loader/LimeLoader` / `Lime-Loader/LimeLoaderInstaller`. The
`melon_data` release-asset name and the on-disk `MelonLoader` folder were intentionally kept
(the installer and Rust bootstrap depend on them). See `SETUP.md` for the end-to-end build,
packaging, and org-publishing steps.
