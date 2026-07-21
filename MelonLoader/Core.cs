using System;
using System.Diagnostics;
using System.Reflection;
using System.IO;
using bHapticsLib;
using System.Threading;
using System.Text;
using JNISharp.NativeInterface;
using System.Linq;
using System.Drawing;
using MelonLoader.Utils;
using MelonLoader.InternalUtils;
using MelonLoader.Resolver;

#if NET6_0_OR_GREATER
using MelonLoader.CoreClrUtils;
#endif

#pragma warning disable IDE0051 // Prevent the IDE from complaining about private unreferenced methods

namespace MelonLoader
{
    internal static class Core
    {
        private static bool _success = true;

        internal static HarmonyLib.Harmony HarmonyInstance;
        internal static bool Is_ALPHA_PreRelease = false;

        internal static int Initialize()
        {
            var runtimeFolder = Path.GetDirectoryName(Assembly.GetExecutingAssembly().Location)!;
            var runtimeDirInfo = new DirectoryInfo(runtimeFolder);
            MelonEnvironment.MelonLoaderDirectory = runtimeDirInfo.Parent!.FullName;
            MelonEnvironment.GameRootDirectory = runtimeDirInfo.Parent!.Parent!.FullName;
            MelonEnvironment.PackageName = BootstrapInterop.NativeGetPackageName();

            MelonLaunchOptions.Load();
            MelonLogger.Setup();

            IntPtr ptr = BootstrapInterop.NativeGetJavaVM();
            JNI.Initialize(ptr);
            APKAssetManager.Initialize();
            MelonLogger.Msg($"Initialized JNI [build {BuildInfo.Version}, ref-delete deferral, main thread {JNI.MainThreadId}]");

            // [FIX/DIAG] The .NET runtime is hosted (invoked via function pointers), so once
            // Start() returns there is no live foreground managed thread - and the runtime was
            // exit()ing cleanly right after, tearing the process down (the DeleteGlobalRef abort is
            // just Unity's atexit teardown of that exit). Hold a foreground thread so the runtime
            // cannot idle-shutdown. If this stops the exit, that was the cause.
            var keepAlive = new System.Threading.Thread(() =>
            {
                while (true) System.Threading.Thread.Sleep(600000);
            })
            { IsBackground = false, Name = "LimeLoaderKeepAlive" };
            keepAlive.Start();

            APKAssetManager.CopyAdditionalData();

            if (IsBad(MelonEnvironment.PackageName))
                throw new Exception();

#if NET35
            // Disabled for now because of issues
            //Net20Compatibility.TryInstall();
#endif

            MelonUtils.SetupWineCheck();
            Utils.MelonConsole.Init();

            if (MelonUtils.IsUnderWineOrSteamProton())
                Pastel.ConsoleExtensions.Disable();

            Fixes.UnhandledException.Install(AppDomain.CurrentDomain);
            // [DIAG] Pin down the clean process exit that happens right after Start(): log when the
            // runtime tears down and the managed stack that triggered it (empty => native/host exit).
            AppDomain.CurrentDomain.ProcessExit += (_, _) =>
            {
                try { MelonLogger.WriteLogToFile($"[DIAG] ProcessExit fired. Managed stack:\n{Environment.StackTrace}"); } catch { }
            };
            AppDomain.CurrentDomain.UnhandledException += (_, e) =>
            {
                try { MelonLogger.WriteLogToFile($"[DIAG] UnhandledException (terminating={e.IsTerminating}): {e.ExceptionObject}"); } catch { }
            };
            Fixes.ServerCertificateValidation.Install();
            Assertions.LemonAssertMapping.Setup();

            MelonUtils.Setup(AppDomain.CurrentDomain);
            BootstrapInterop.SetDefaultConsoleTitleWithGameName(UnityInformationHandler.GameName, 
                UnityInformationHandler.GameVersion);

            MelonAssemblyResolver.Setup();

#if NET6_0_OR_GREATER

            if (MelonLaunchOptions.Core.UserWantsDebugger && MelonEnvironment.IsDotnetRuntime)
            {
                MelonLogger.Msg("[Init] User requested debugger, attempting to launch now...");
                Debugger.Launch();
            }

            Environment.SetEnvironmentVariable("IL2CPP_INTEROP_DATABASES_LOCATION", MelonEnvironment.Il2CppAssembliesDirectory);

#else

            try
            {
                if (!MonoLibrary.Setup())
                {
                    _success = false;
                    return 1;
                }
            }
            catch (Exception ex)
            {
                MelonDebug.Msg($"[MonoLibrary] Caught Exception: {ex}");
                _success = false;
                return 1;
            }

#endif

            MonoMod.Logs.DebugLog.OnLog += (string source, DateTime time, MonoMod.Logs.LogLevel level, string message) => MelonDebug.Msg($"[MonoMod] [{source}] [{level}] {message}");

            HarmonyInstance = new HarmonyLib.Harmony(BuildInfo.Name);
            
#if NET6_0_OR_GREATER
            // if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
            //  NativeStackWalk.LogNativeStackTrace();

            Fixes.DotnetAssemblyLoadContextFix.Install();
            Fixes.DotnetModHandlerRedirectionFix.Install();
#endif

            Fixes.ForcedCultureInfo.Install();
            Fixes.InstancePatchFix.Install();
            Fixes.ProcessFix.Install();

#if NET6_0_OR_GREATER
            Fixes.Il2CppInteropFixes.Install();

            // DISABLED on Unity 6 / Quest: this hooks il2cpp_resolve_icall, and the detour runs managed
            // code on whatever thread resolves an icall - including Unity worker threads. Unity 6 ships
            // engine modules whose managed methods are stripped (UnityEngine.AccessibilityModule is the
            // one that bites here): the original resolve returns null, so the detour falls through to
            // ShouldInject, finds the matching type in our loaded interop assemblies, and then generates
            // IL / calls into Il2CppInterop from that worker thread. Result is a SIGSEGV with an empty
            // managed stacktrace, ~2ms after Unity logs its AccessibilityModule resolution failures,
            // followed by the DeleteGlobalRef teardown abort. Injected icalls are a mod convenience, not
            // required for loading, so leave this off until the detour is made thread/runtime safe.
            //Fixes.Il2CppICallInjector.Install();
#endif

            PatchShield.Install();

            MelonPreferences.Load();

            MelonCompatibilityLayer.LoadModules();
            
            MelonHandler.LoadUserlibs(MelonEnvironment.UserLibsDirectory);
            MelonHandler.LoadMelonFolders<MelonPlugin>(MelonEnvironment.PluginsDirectory);

            MelonEvents.MelonHarmonyEarlyInit.Invoke();
            MelonEvents.OnPreInitialization.Invoke();

            return 0;
        }

        internal static int PreStart()
        {
            MelonEvents.OnApplicationEarlyStart.Invoke();
            int result = PreSetup();
#if NET6_0_OR_GREATER
            if (result == 0)
                PreloadIl2CppAssemblies();
#endif
            return result;
        }

        private static int PreSetup()
        {
#if NET6_0_OR_GREATER
            if (_success)
                _success = Il2CppAssemblyGenerator.Run();
#endif

            return _success ? 0 : 1;
        }

#if NET6_0_OR_GREATER
        // Pre-load the generated il2cpp interop assemblies now - PreStart runs during il2cpp_init,
        // before Unity's first scene renders. Otherwise the support module loads all ~144 DLLs during
        // the scene-change hook, stalling the Unity main thread at the render-critical moment, which
        // crashed Unity 6's Vulkan framebuffer setup on Quest. Assembly.LoadFrom only reads metadata
        // (il2cpp binding stays lazy), so this is safe to do early and makes the support module's own
        // LoadFrom loop effectively a no-op.
        private static void PreloadIl2CppAssemblies()
        {
            try
            {
                string dir = MelonEnvironment.Il2CppAssembliesDirectory;
                if (!Directory.Exists(dir))
                    return;

                int loaded = 0;
                foreach (string file in Directory.GetFiles(dir, "*.dll"))
                {
                    try { Assembly.LoadFrom(file); loaded++; }
                    catch { }
                }
                MelonLogger.Msg($"Preloaded {loaded} Il2Cpp interop assemblies");
            }
            catch (Exception ex)
            {
                MelonDebug.Msg($"Il2Cpp interop assembly preload failed: {ex}");
            }
        }
#endif

        internal static int Start()
        {
            if (!_success)
                return 1;

            MelonEvents.OnPreModsLoaded.Invoke();
            MelonHandler.LoadMelonFolders<MelonMod>(MelonEnvironment.ModsDirectory);

            MelonEvents.OnPreSupportModule.Invoke();
            if (!SupportModule.Setup())
                return 1;
            MelonLogger.WriteLogToFile("[DIAG] Start: after SupportModule.Setup");

            AddUnityDebugLog();
            MelonLogger.WriteLogToFile("[DIAG] Start: after AddUnityDebugLog");

#if NET6_0_OR_GREATER
            RegisterTypeInIl2Cpp.SetReady();
            RegisterTypeInIl2CppWithInterfaces.SetReady();
#endif
            MelonLogger.WriteLogToFile("[DIAG] Start: after RegisterTypeInIl2Cpp.SetReady");

            MelonEvents.MelonHarmonyInit.Invoke();
            MelonLogger.WriteLogToFile("[DIAG] Start: after MelonHarmonyInit");
            MelonEvents.OnApplicationStart.Invoke();
            MelonLogger.WriteLogToFile("[DIAG] Start: after OnApplicationStart - Start() complete");

            return 0;
        }
        
        internal static string GetVersionString()
        {
            var versionStr = $"{BuildInfo.Name} " +
                             $"v{BuildInfo.Version} " +
                             $"{(Is_ALPHA_PreRelease ? "ALPHA Pre-Release" : "Open-Beta")}";
            return versionStr;
        }
        
        internal static void WelcomeMessage()
        {
            //if (MelonDebug.IsEnabled())
            //    MelonLogger.WriteSpacer();

            MelonLogger.MsgDirect("------------------------------");
            MelonLogger.MsgDirect(GetVersionString());
            MelonLogger.MsgDirect($"OS: {MelonUtils.GetOSVersion()}");
            MelonLogger.MsgDirect($"Hash Code: {MelonUtils.HashCode}");
            MelonLogger.MsgDirect("------------------------------");
            var typeString = MelonUtils.IsGameIl2Cpp() ? "Il2cpp" : MelonUtils.IsOldMono() ? "Mono" : "MonoBleedingEdge";
            MelonLogger.MsgDirect($"Game Type: {typeString}");
            var archString = MelonUtils.IsGame32Bit() ? "x86" : "x64";
            MelonLogger.MsgDirect($"Game Arch: {archString}");
            MelonLogger.MsgDirect("------------------------------");
            MelonLogger.MsgDirect("Command-Line: ");
            foreach (var pair in MelonLaunchOptions.InternalArguments)
                if (string.IsNullOrEmpty(pair.Value))
                    MelonLogger.MsgDirect($"   {pair.Key}");
                else
                    MelonLogger.MsgDirect($"   {pair.Key} = {pair.Value}");
            MelonLogger.MsgDirect("------------------------------");
            MelonEnvironment.PrintEnvironment();
        }
        
        internal static void Quit()
        {
            MelonDebug.Msg("[ML Core] Received Quit Request! Shutting down...");
            
            MelonPreferences.Save();

            HarmonyInstance.UnpatchSelf();
            bHapticsManager.Disconnect();

#if NET6_0_OR_GREATER
            Fixes.Il2CppInteropFixes.Shutdown();
            Fixes.Il2CppICallInjector.Shutdown();
#endif

            MelonLogger.Flush();
            //MelonLogger.Close();

            Thread.Sleep(200);

            if (MelonLaunchOptions.Core.QuitFix)
                Process.GetCurrentProcess().Kill();
        }

        private static void AddUnityDebugLog()
        {
            var msg = "~   This Game has been MODIFIED using MelonLoader. DO NOT report any issues to the Game Developers!   ~";
            var line = new string('-', msg.Length);
            SupportModule.Interface.UnityDebugLog(line);
            SupportModule.Interface.UnityDebugLog(msg);
            SupportModule.Interface.UnityDebugLog(line);
        }

        private static bool IsBad(this string self)
        {
#if NET6_0_OR_GREATER
            byte[] stringBytes = Encoding.UTF8.GetBytes(self);
            byte[] hashBytes = System.Security.Cryptography.MD5.HashData(stringBytes);
            StringBuilder sb = new();
            for (int i = 0; i < hashBytes.Length; i++)
                sb.Append(hashBytes[i].ToString("x2"));
            string hash = sb.ToString();

            return _bad.Contains(hash);
#else
            return true; // unreachable theoretically
#endif
        }


        private static readonly string[] _bad = [
            "95fb4cd16729627d013dc620a807c23c",
            "ffaf599e1b7e1175cd344b367e4a7ec4",
            "be1878f1900f48586eb7cab537f82f62",
            "196d46a42878aae4188839d35fdad747",
            "9b6f24bad02220abf7e12d7b4ad771f4",
            "a5595fbc343dbc2a468eb76533d345a5",
            "964c753427382e3bf56c1f7ee5a37f06",
            "e010d19cbf15c335d8f1852a1639c42c",
            "72cfa3439d21cc03ece7182cd494b75b",
            "0a4876540f4f7a11fd57a6ce54bbe0a7",
            "79aca3897e0c3e750a1f4b62776e8831",
            "f913df2dc82284a4689b8504bceb8241",
            "8239c5431b7656ab0e67ac78e6f807ff",
            "f810acd7cd40c97dfed703466476ceaa",
            "9396d377bfe52476013d5b007cfc19bf",
            "e56e5d1be5311620015cb070d11802ab",
            "6e8b0ebfaa80d548e3ee647281cbc627",
            "8e58816bb80589b154b77d84629f5a9e",
        ];
    }
}