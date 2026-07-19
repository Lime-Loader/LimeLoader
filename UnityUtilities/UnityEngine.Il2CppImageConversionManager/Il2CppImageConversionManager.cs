using Il2CppInterop.Runtime;
using Il2CppInterop.Runtime.InteropTypes.Arrays;
using MelonLoader.InternalUtils;
using System;
using System.Runtime.InteropServices;

namespace UnityEngine;

public class Il2CppImageConversionManager
{
    // Implemented class/struct needed for unmarshalling of data in Unity 6.
    // New struct needed for Unity 6 function calls.
    [StructLayout(LayoutKind.Sequential)]
    public struct ManagedSpanWrapper
    {
        public unsafe void* begin;
        public int length;
    }
    internal static class BindingsAllocator
    {
        public struct NativeOwnedMemory
        {
            public unsafe void* data;
        }

        static BindingsAllocator()
        {
            FreeNativeOwnedMemoryDelegateField = IL2CPP.ResolveICall<FreeNativeOwnedMemoryDelegate>("UnityEngine.Bindings.BindingsAllocator::FreeNativeOwnedMemory");
        }

        public unsafe static void FreeNativeOwnedMemory(void* ptr) => FreeNativeOwnedMemoryDelegateField(ptr);

        public unsafe static void* GetNativeOwnedDataPointer(void* ptr)
        {
            return ((NativeOwnedMemory*)ptr)->data;
        }

        unsafe delegate void FreeNativeOwnedMemoryDelegate(void* ptr);
        static FreeNativeOwnedMemoryDelegate FreeNativeOwnedMemoryDelegateField;
    }
    public ref struct BlittableArrayWrapper
    {
        enum UpdateFlags
        {
            NoUpdateNeeded,
            SizeChanged,
            DataIsNativePointer,
            DataIsNativeOwnedMemory,
            DataIsEmpty,
            DataIsNull
        }

        unsafe void* data;
        int size;
        UpdateFlags updateFlags;

        public unsafe void Unmarshal<T>(ref T[] array) where T : struct
        {
            switch (updateFlags)
            {
                case BlittableArrayWrapper.UpdateFlags.SizeChanged:
                case BlittableArrayWrapper.UpdateFlags.DataIsNativePointer:
                    array = new Span<T>(data, size).ToArray();
                    break;
                case BlittableArrayWrapper.UpdateFlags.DataIsNativeOwnedMemory:
                    array = new Span<T>(BindingsAllocator.GetNativeOwnedDataPointer(data), size).ToArray();
                    BindingsAllocator.FreeNativeOwnedMemory(data);
                    break;
                case BlittableArrayWrapper.UpdateFlags.DataIsEmpty:
                    array = Array.Empty<T>();
                    break;
                case BlittableArrayWrapper.UpdateFlags.DataIsNull:
                    array = null;
                    break;
            }
        }
    }

    static Il2CppImageConversionManager()
    {
        if (UnityInformationHandler.EngineVersion.Major >= 6000)
        {
            EncodeToTGADelegateField_Unity6 = IL2CPP.ResolveICall<TextureOnlyDelegate_Unity6>("UnityEngine.ImageConversion::EncodeToTGA_Injected");
            EncodeToEXRDelegateField_Unity6 = IL2CPP.ResolveICall<TextureAndFlagDelegate_Unity6>("UnityEngine.ImageConversion::EncodeToEXR_Injected");
            EncodeToPNGDelegateField_Unity6 = IL2CPP.ResolveICall<TextureOnlyDelegate_Unity6>("UnityEngine.ImageConversion::EncodeToPNG_Injected");
            EncodeToJPGDelegateField_Unity6 = IL2CPP.ResolveICall<TextureAndQualityDelegate_Unity6>("UnityEngine.ImageConversion::EncodeToJPG_Injected");
            LoadImageDelegateField_Unity6 = IL2CPP.ResolveICall<LoadImageDelegate_Unity6>("UnityEngine.ImageConversion::LoadImage_Injected");
        }
        else
        {
            EncodeToTGADelegateField = IL2CPP.ResolveICall<TextureOnlyDelegate>("UnityEngine.ImageConversion::EncodeToTGA");
            EncodeToEXRDelegateField = IL2CPP.ResolveICall<TextureAndFlagDelegate>("UnityEngine.ImageConversion::EncodeToEXR");
            EncodeToPNGDelegateField = IL2CPP.ResolveICall<TextureOnlyDelegate>("UnityEngine.ImageConversion::EncodeToPNG");
            EncodeToJPGDelegateField = IL2CPP.ResolveICall<TextureAndQualityDelegate>("UnityEngine.ImageConversion::EncodeToJPG");
            LoadImageDelegateField = IL2CPP.ResolveICall<LoadImageDelegate>("UnityEngine.ImageConversion::LoadImage");
        }
    }

    public static Il2CppStructArray<byte> EncodeToTGA(Texture2D tex)
    {
        if (tex == null)
            throw new ArgumentException("The texture cannot be null.");
        if (UnityInformationHandler.EngineVersion.Major >= 6000)
        {
            if (EncodeToTGADelegateField_Unity6 == null)
                throw new NullReferenceException("The EncodeToTGADelegateField_Unity6 cannot be null.");

            EncodeToTGADelegateField_Unity6(tex.m_CachedPtr, out BlittableArrayWrapper arrayWrapper);
            byte[] array = null;
            arrayWrapper.Unmarshal<byte>(ref array);

            return array;
        }
        else
        {
            if (EncodeToTGADelegateField == null)
                throw new NullReferenceException("The EncodeToTGADelegateField cannot be null.");

            var arrayPtr = EncodeToTGADelegateField(IL2CPP.Il2CppObjectBaseToPtr(tex));
            if (arrayPtr != IntPtr.Zero)
                return new Il2CppStructArray<byte>(arrayPtr);
            else
                return null;
        }
    }

    public static Il2CppStructArray<byte> EncodeToPNG(Texture2D tex)
    {
        if (tex == null)
            throw new ArgumentException("The texture cannot be null.");
        if (UnityInformationHandler.EngineVersion.Major >= 6000)
        {
            if (EncodeToPNGDelegateField_Unity6 == null)
                throw new NullReferenceException("The EncodeToPNGDelegateField_Unity6 cannot be null.");

            EncodeToPNGDelegateField_Unity6(tex.m_CachedPtr, out BlittableArrayWrapper arrayWrapper);
            byte[] array = null;
            arrayWrapper.Unmarshal<byte>(ref array);

            return array;
        }
        else
        {
            if (EncodeToPNGDelegateField == null)
                throw new NullReferenceException("The EncodeToPNGDelegateField cannot be null.");

            var arrayPtr = EncodeToPNGDelegateField(IL2CPP.Il2CppObjectBaseToPtr(tex));
            if (arrayPtr != IntPtr.Zero)
                return new Il2CppStructArray<byte>(arrayPtr);
            else
                return null;
        }
    }

    public static Il2CppStructArray<byte> EncodeToJPG(Texture2D tex, int quality)
    {
        if (tex == null)
            throw new ArgumentException("The texture cannot be null.");
        if (UnityInformationHandler.EngineVersion.Major >= 6000)
        {
            if (EncodeToJPGDelegateField_Unity6 == null)
                throw new NullReferenceException("The EncodeToJPGDelegateField_Unity6 cannot be null.");

            EncodeToJPGDelegateField_Unity6(tex.m_CachedPtr, quality, out BlittableArrayWrapper arrayWrapper);
            byte[] array = null;
            arrayWrapper.Unmarshal<byte>(ref array);

            return array;
        }
        else
        {
            if (EncodeToJPGDelegateField == null)
                throw new NullReferenceException("The EncodeToJPGDelegateField cannot be null.");

            var arrayPtr = EncodeToJPGDelegateField(IL2CPP.Il2CppObjectBaseToPtr(tex), quality);
            if (arrayPtr != IntPtr.Zero)
                return new Il2CppStructArray<byte>(arrayPtr);
            else
                return null;
        }
    }
    public static Il2CppStructArray<byte> EncodeToJPG(Texture2D tex) => EncodeToJPG(tex, 75);

    public static Il2CppStructArray<byte> EncodeToEXR(Texture2D tex, Texture2D.EXRFlags flags)
    {
        if (tex == null)
            throw new ArgumentException("The texture cannot be null.");
        if (UnityInformationHandler.EngineVersion.Major >= 6000)
        {
            if (EncodeToEXRDelegateField_Unity6 == null)
                throw new NullReferenceException("The EncodeToEXRDelegateField_Unity6 cannot be null.");

            EncodeToEXRDelegateField_Unity6(tex.m_CachedPtr, flags, out BlittableArrayWrapper arrayWrapper);
            byte[] array = null;
            arrayWrapper.Unmarshal<byte>(ref array);

            return array;
        }
        else
        {
            if (EncodeToEXRDelegateField == null)
                throw new NullReferenceException("The EncodeToEXRDelegateField cannot be null.");

            var arrayPtr = EncodeToEXRDelegateField(IL2CPP.Il2CppObjectBaseToPtr(tex), flags);
            if (arrayPtr != IntPtr.Zero)
                return new Il2CppStructArray<byte>(arrayPtr);
            else
                return null;
        }
    }
    public static Il2CppStructArray<byte> EncodeToEXR(Texture2D tex) => EncodeToEXR(tex, 0);

    public static bool LoadImage(Texture2D tex, Il2CppStructArray<byte> data, bool markNonReadable)
    {
        if (tex == null)
            throw new ArgumentException("The texture cannot be null.");
        if (data == null)
            throw new ArgumentException("The data cannot be null.");

        if (UnityInformationHandler.EngineVersion.Major >= 6000)
        {
            if (LoadImageDelegateField_Unity6 == null)
                throw new NullReferenceException("The LoadImageDelegateField_Unity6 cannot be null.");

            unsafe
            {
                var arrayPtr = IL2CPP.Il2CppObjectBaseToPtr(data);
                var span = new ManagedSpanWrapper
                {
                    begin = (void*)(arrayPtr + 0x20), // Skip the IL2CPP object header.
                    length = data.Length
                };
                return LoadImageDelegateField_Unity6(tex.m_CachedPtr, ref span, markNonReadable);
            }
        }
        else
        {
            if (LoadImageDelegateField == null)
                throw new NullReferenceException("The LoadImageDelegateField cannot be null.");
            return LoadImageDelegateField(IL2CPP.Il2CppObjectBaseToPtr(tex), IL2CPP.Il2CppObjectBaseToPtr(data), markNonReadable);
        }
    }
    public static bool LoadImage(Texture2D tex, Il2CppStructArray<byte> data) => LoadImage(tex, data, false);

    private delegate IntPtr TextureOnlyDelegate(IntPtr tex);
    private delegate IntPtr TextureAndQualityDelegate(IntPtr tex, int quality);
    private delegate IntPtr TextureAndFlagDelegate(IntPtr tex, Texture2D.EXRFlags flags);
    private delegate bool LoadImageDelegate(IntPtr tex, IntPtr data, bool markNonReadable);
    private readonly static TextureAndFlagDelegate EncodeToEXRDelegateField;
    private readonly static TextureOnlyDelegate EncodeToTGADelegateField;
    private readonly static TextureOnlyDelegate EncodeToPNGDelegateField;
    private readonly static TextureAndQualityDelegate EncodeToJPGDelegateField;
    private readonly static LoadImageDelegate LoadImageDelegateField;
    // ----------
    private delegate void TextureOnlyDelegate_Unity6(IntPtr tex, out BlittableArrayWrapper ret);
    private delegate void TextureAndQualityDelegate_Unity6(IntPtr tex, int quality, out BlittableArrayWrapper ret);
    private delegate void TextureAndFlagDelegate_Unity6(IntPtr tex, Texture2D.EXRFlags flags, out BlittableArrayWrapper ret);
    private delegate bool LoadImageDelegate_Unity6(IntPtr tex, ref ManagedSpanWrapper data, bool markNonReadable);
    private readonly static TextureAndFlagDelegate_Unity6 EncodeToEXRDelegateField_Unity6;
    private readonly static TextureOnlyDelegate_Unity6 EncodeToTGADelegateField_Unity6;
    private readonly static TextureOnlyDelegate_Unity6 EncodeToPNGDelegateField_Unity6;
    private readonly static TextureAndQualityDelegate_Unity6 EncodeToJPGDelegateField_Unity6;
    private readonly static LoadImageDelegate_Unity6 LoadImageDelegateField_Unity6;
}