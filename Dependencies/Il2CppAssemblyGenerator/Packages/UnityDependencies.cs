using System.Collections.Generic;
using System.IO;

namespace MelonLoader.Il2CppAssemblyGenerator.Packages
{
    internal class UnityDependencies : Models.PackageBase
    {
        private readonly string requestedVersion;

        internal UnityDependencies()
        {
            Name = nameof(UnityDependencies);
            Version = InternalUtils.UnityInformationHandler.EngineVersion.ToStringWithoutType();
            requestedVersion = Version;
            URL = $"https://github.com/LavaGang/Unity-Runtime-Libraries/raw/master/{Version}.zip";
            Destination = Path.Combine(Core.BasePath, Name);
            FilePath = Path.Combine(Core.BasePath, $"{Name}_{Version}.zip");
        }

        internal override bool ShouldSetup()
            => string.IsNullOrEmpty(Config.Values.UnityVersion)
            || !Config.Values.UnityVersion.Equals(Version);

        internal override void Save()
            => Save(ref Config.Values.UnityVersion);

        // The newest Unity versions aren't hosted in Unity-Runtime-Libraries yet, so the exact
        // version usually 404s. Try the exact version, then progressively nearer fallbacks, and if
        // none can be fetched, continue WITHOUT unity base libraries (Il2CppInterop treats them as
        // optional) instead of failing assembly generation outright.
        internal override bool Setup()
        {
            if (!ShouldSetup())
            {
                Core.Logger.Msg($"{Name} is up to date.");
                return true;
            }

            Core.AssemblyGenerationNeeded = true;

            foreach (string candidate in GetCandidateVersions())
            {
                string url = $"https://github.com/LavaGang/Unity-Runtime-Libraries/raw/master/{candidate}.zip";
                string filePath = Path.Combine(Core.BasePath, $"{Name}_{candidate}.zip");

                if (!File.Exists(filePath))
                {
                    if (MelonLaunchOptions.Il2CppAssemblyGenerator.OfflineMode)
                        continue;

                    Core.Logger.Msg($"Downloading {Name} ({candidate})...");
                    if (!FileHandler.Download(url, filePath))
                        continue;
                }

                Version = candidate;
                URL = url;
                FilePath = filePath;

                Core.Logger.Msg($"Processing {Name} ({candidate})...");
                if (!OnProcess())
                {
                    try { File.Delete(filePath); } catch { /* best effort */ }
                    continue;
                }

                if (candidate != requestedVersion)
                    Core.Logger.Warning($"Unity base libraries for {requestedVersion} are unavailable; using the nearest hosted version {candidate} instead.");

                Save();
                return true;
            }

            Core.Logger.Warning($"No Unity base libraries available for {requestedVersion} (or a nearby version). Continuing without them - generated interop assemblies may be less complete.");

            // Leave an (empty) destination so downstream Directory.GetFiles calls don't throw, and
            // record the version so we don't re-attempt the download every launch.
            try { Directory.CreateDirectory(Destination); } catch { /* best effort */ }
            Version = requestedVersion;
            Save();
            return true;
        }

        // Exact version, then the minor baseline (x.y.0) and major baseline (x.0.0), which are the
        // versions most likely to actually be hosted for a bleeding-edge Unity build.
        private IEnumerable<string> GetCandidateVersions()
        {
            yield return requestedVersion;

            string[] parts = requestedVersion.Split('.');
            if (parts.Length >= 3)
            {
                string minorBaseline = $"{parts[0]}.{parts[1]}.0";
                if (minorBaseline != requestedVersion)
                    yield return minorBaseline;

                string majorBaseline = $"{parts[0]}.0.0";
                if (majorBaseline != requestedVersion && majorBaseline != minorBaseline)
                    yield return majorBaseline;
            }
        }
    }
}
