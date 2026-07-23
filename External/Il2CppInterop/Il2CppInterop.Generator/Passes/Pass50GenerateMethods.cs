using AsmResolver.DotNet;
using AsmResolver.DotNet.Code.Cil;
using AsmResolver.DotNet.Signatures;
using AsmResolver.PE.DotNet.Cil;
using Il2CppInterop.Generator.Contexts;
using Il2CppInterop.Generator.Extensions;
using Il2CppInterop.Generator.Utils;

namespace Il2CppInterop.Generator.Passes;

public static class Pass50GenerateMethods
{
    public static void DoPass(RewriteGlobalContext context)
    {
        foreach (var assemblyContext in context.Assemblies)
        {
            foreach (var typeContext in assemblyContext.Types)
            {
                foreach (var methodRewriteContext in typeContext.Methods)
                {
                    var originalMethod = methodRewriteContext.OriginalMethod;
                    var newMethod = methodRewriteContext.NewMethod;
                    var imports = assemblyContext.Imports;

                    var bodyBuilder = newMethod.CilMethodBody!.Instructions;
                    var exceptionLocal = new CilLocalVariable(imports.Module.IntPtr());
                    var argArray = new CilLocalVariable(imports.Module.IntPtr().MakePointerType());
                    var resultVar = new CilLocalVariable(imports.Module.IntPtr());
                    var valueTypeLocal = new CilLocalVariable(newMethod.Signature!.ReturnType);
                    newMethod.CilMethodBody.LocalVariables.Add(exceptionLocal);
                    newMethod.CilMethodBody.LocalVariables.Add(argArray);
                    newMethod.CilMethodBody.LocalVariables.Add(resultVar);

                    if (valueTypeLocal.VariableType.FullName != "System.Void")
                        newMethod.CilMethodBody.LocalVariables.Add(valueTypeLocal);

                    // --- Direct native-call path for multi-field blittable value-type getters ---
                    // il2cpp_runtime_invoke boxes the return value, and on arm64 that boxing corrupts
                    // homogeneous-float aggregates (Vector2/3/4, Quaternion, Color, ...) so every
                    // component reads back equal to the first (the transform.position (x,x,x) bug).
                    // For a parameterless, non-virtual method whose return is such a struct, call the
                    // native method pointer directly instead - CoreCLR returns the HFA correctly.
                    // Restricted to >=2-field blittable structs: single value returns don't corrupt, and
                    // keeping them on runtime_invoke preserves its il2cpp exception handling.
                    if (TryEmitDirectValueTypeGetter(context, bodyBuilder, methodRewriteContext, typeContext, imports))
                        continue;

                    // Pre-initialize any present params
                    // TODO: This doesn't account for params T[] (i.e. generic element type) yet; may emit incorrect IL
                    // TODO: Do we really need a loop here? C# allows only one params array.
                    //       On the other hand, CreateParamsMethod accommodates multiple ParamArrayAttribute as well
                    CilInstructionLabel? nextInstruction = null;
                    for (var paramIndex = 0; paramIndex < originalMethod.Parameters.Count; paramIndex++)
                    {
                        var newParameter = newMethod.Parameters[paramIndex];
                        var originalParameter = originalMethod.Parameters[paramIndex];
                        if (!originalParameter.IsParamsArray())
                            continue;

                        var originalElementType = ((ArrayBaseTypeSignature)originalParameter.ParameterType).BaseType;

                        if (nextInstruction != null)
                            nextInstruction.Instruction = bodyBuilder.Add(OpCodes.Nop);
                        nextInstruction = new();

                        bodyBuilder.Add(OpCodes.Ldarg, newParameter);
                        bodyBuilder.Add(OpCodes.Brtrue, nextInstruction);

                        bodyBuilder.Add(OpCodes.Ldc_I4_0);
                        bodyBuilder.Add(OpCodes.Conv_I8);
                        bodyBuilder.Add(OpCodes.Newobj, imports.Module.DefaultImporter.ImportMethod(originalElementType.FullName switch
                        {
                            "System.String" => imports.Il2CppStringArrayctor_size.Value,
                            _ when originalElementType.IsValueType => imports.Il2CppStructArrayctor_size.Get(((GenericInstanceTypeSignature)newParameter.ParameterType).TypeArguments[0]),
                            _ => imports.Il2CppRefrenceArrayctor_size.Get(((GenericInstanceTypeSignature)newParameter.ParameterType).TypeArguments[0])
                        }));
                        bodyBuilder.Add(OpCodes.Starg, newParameter);
                    }

                    if (nextInstruction != null)
                        nextInstruction.Instruction = bodyBuilder.Add(OpCodes.Nop);

                    if (typeContext.ComputedTypeSpecifics != TypeRewriteContext.TypeSpecifics.BlittableStruct)
                    {
                        if (originalMethod.IsConstructor)
                        {
                            bodyBuilder.Add(OpCodes.Ldarg_0);
                            bodyBuilder.Add(OpCodes.Ldsfld, typeContext.ClassPointerFieldRef);
                            bodyBuilder.Add(OpCodes.Call, imports.IL2CPP_il2cpp_object_new.Value);
                            bodyBuilder.Add(OpCodes.Call,
                                ReferenceCreator.CreateInstanceMethodReference(".ctor", imports.Module.Void(), typeContext.SelfSubstitutedRef, imports.Module.IntPtr()));
                        }
                        else if (!originalMethod.IsStatic)
                        {
                            bodyBuilder.Add(OpCodes.Ldarg_0);
                            bodyBuilder.Add(OpCodes.Call, imports.IL2CPP_Il2CppObjectBaseToPtrNotNull.Value);
                            bodyBuilder.Add(OpCodes.Pop);
                        }
                    }

                    if (originalMethod.Parameters.Count == 0)
                    {
                        bodyBuilder.Add(OpCodes.Ldc_I4_0);
                        bodyBuilder.Add(OpCodes.Conv_U);
                    }
                    else
                    {
                        bodyBuilder.Add(OpCodes.Ldc_I4, originalMethod.Parameters.Count);
                        bodyBuilder.Add(OpCodes.Conv_U);
                        bodyBuilder.Add(OpCodes.Sizeof, imports.Module.IntPtr().ToTypeDefOrRef());
                        bodyBuilder.Add(OpCodes.Mul_Ovf_Un);
                        bodyBuilder.Add(OpCodes.Localloc);
                    }

                    bodyBuilder.Add(OpCodes.Stloc, argArray);

                    var argOffset = originalMethod.IsStatic ? 0 : 1;

                    var byRefParams = new List<(int, CilLocalVariable)>();

                    for (var i = 0; i < newMethod.Parameters.Count; i++)
                    {
                        bodyBuilder.Add(OpCodes.Ldloc, argArray);
                        if (i > 0)
                        {
                            bodyBuilder.Add(OpCodes.Ldc_I4, i);
                            bodyBuilder.Add(OpCodes.Conv_U);
                            bodyBuilder.Add(OpCodes.Sizeof, imports.Module.IntPtr().ToTypeDefOrRef());
                            bodyBuilder.Add(OpCodes.Mul_Ovf_Un);
                            bodyBuilder.Add(OpCodes.Add);
                        }

                        var newParam = newMethod.Parameters[i];
                        // NOTE(Kas): out parameters of value type are passed directly as a pointer to the il2cpp method
                        // since we don't need to perform any additional copies
                        if (newParam.Definition!.IsOut && !newParam.ParameterType.GetElementType().IsValueType)
                        {
                            var elementType = newParam.ParameterType.GetElementType();

                            // Storage for the output Il2CppObjectBase pointer, it's
                            // unused if there's a generic value type parameter
                            var outVar = new CilLocalVariable(imports.Module.IntPtr());
                            bodyBuilder.Owner.LocalVariables.Add(outVar);

                            if (elementType is GenericParameterSignature)
                            {
                                bodyBuilder.Add(OpCodes.Ldtoken, elementType.ToTypeDefOrRef());
                                bodyBuilder.Add(OpCodes.Call, imports.Module.TypeGetTypeFromHandle());
                                bodyBuilder.Add(OpCodes.Callvirt, imports.Module.TypeGetIsValueType());

                                var valueTypeBlock = new CilInstructionLabel();
                                var continueBlock = new CilInstructionLabel();

                                bodyBuilder.Add(OpCodes.Brtrue, valueTypeBlock);

                                // The generic parameter is an Il2CppObjectBase => set the output storage to a nullptr
                                bodyBuilder.Add(OpCodes.Ldc_I4, 0);
                                bodyBuilder.Add(OpCodes.Stloc, outVar);
                                bodyBuilder.Add(OpCodes.Ldloca, outVar);
                                bodyBuilder.Add(OpCodes.Conv_I);

                                bodyBuilder.Add(OpCodes.Br_S, continueBlock);

                                // Instruction block that handles generic value types, we only need to return a reference
                                // to the output argument since it is already allocated for us
                                valueTypeBlock.Instruction = bodyBuilder.Add(OpCodes.Nop);
                                bodyBuilder.AddLoadArgument(argOffset + i);

                                continueBlock.Instruction = bodyBuilder.Add(OpCodes.Nop);
                            }
                            else
                            {
                                bodyBuilder.Add(OpCodes.Ldc_I4, 0);
                                bodyBuilder.Add(OpCodes.Stloc, outVar);
                                bodyBuilder.Add(OpCodes.Ldloca, outVar);
                                bodyBuilder.Add(OpCodes.Conv_I);
                            }
                            byRefParams.Add((i, outVar));
                        }
                        else
                        {
                            bodyBuilder.EmitObjectToPointer(originalMethod.Parameters[i].ParameterType, newParam.ParameterType,
                                methodRewriteContext.DeclaringType, argOffset + i, false, true, true, false, out var refVar);
                            if (refVar != null)
                                byRefParams.Add((i, refVar));
                        }
                        bodyBuilder.Add(OpCodes.Stind_I);

                    }

                    if (!originalMethod.DeclaringType!.IsSealed && !originalMethod.IsFinal &&
                        ((originalMethod.IsVirtual && !originalMethod.DeclaringType.IsValueType) || originalMethod.IsAbstract))
                    {
                        bodyBuilder.Add(OpCodes.Ldarg_0);
                        bodyBuilder.Add(OpCodes.Call, imports.IL2CPP_Il2CppObjectBaseToPtr.Value);
                        if (methodRewriteContext.GenericInstantiationsStoreSelfSubstRef != null)
                            bodyBuilder.Add(OpCodes.Ldsfld,
                                ReferenceCreator.CreateFieldReference("Pointer", imports.Module.IntPtr(),
                                    methodRewriteContext.GenericInstantiationsStoreSelfSubstMethodRef));
                        else
                            bodyBuilder.Add(OpCodes.Ldsfld, methodRewriteContext.NonGenericMethodInfoPointerField);
                        bodyBuilder.Add(OpCodes.Call, imports.IL2CPP_il2cpp_object_get_virtual_method.Value);
                    }
                    else if (methodRewriteContext.GenericInstantiationsStoreSelfSubstRef != null)
                    {
                        bodyBuilder.Add(OpCodes.Ldsfld,
                            ReferenceCreator.CreateFieldReference("Pointer", imports.Module.IntPtr(),
                                methodRewriteContext.GenericInstantiationsStoreSelfSubstMethodRef));
                    }
                    else
                    {
                        bodyBuilder.Add(OpCodes.Ldsfld, methodRewriteContext.NonGenericMethodInfoPointerField);
                    }

                    if (originalMethod.IsStatic)
                        bodyBuilder.Add(OpCodes.Ldc_I4_0);
                    else
                        bodyBuilder.EmitObjectToPointer(originalMethod.DeclaringType.ToTypeSignature(), newMethod.DeclaringType!.ToTypeSignature(), typeContext, 0,
                            true, false, true, true, out _);

                    bodyBuilder.Add(OpCodes.Ldloc, argArray);
                    bodyBuilder.Add(OpCodes.Ldloca, exceptionLocal);
                    bodyBuilder.Add(OpCodes.Call, imports.IL2CPP_il2cpp_runtime_invoke.Value);
                    bodyBuilder.Add(OpCodes.Stloc, resultVar);

                    bodyBuilder.Add(OpCodes.Ldloc, exceptionLocal);
                    bodyBuilder.Add(OpCodes.Call, imports.Il2CppException_RaiseExceptionIfNecessary.Value);

                    foreach (var byRefParam in byRefParams)
                    {
                        var paramIndex = byRefParam.Item1;
                        var paramVariable = byRefParam.Item2;
                        var methodParam = newMethod.Parameters[paramIndex];

                        if (methodParam.Definition!.IsOut && methodParam.ParameterType.GetElementType() is GenericParameterSignature)
                        {
                            bodyBuilder.Add(OpCodes.Ldtoken, methodParam.ParameterType.GetElementType().ToTypeDefOrRef());
                            bodyBuilder.Add(OpCodes.Call, imports.Module.TypeGetTypeFromHandle());
                            bodyBuilder.Add(OpCodes.Callvirt, imports.Module.TypeGetIsValueType());

                            var continueBlock = new CilInstructionLabel();

                            bodyBuilder.Add(OpCodes.Brtrue, continueBlock);

                            // The generic parameter is an Il2CppObjectBase => update the reference appropriately
                            bodyBuilder.EmitUpdateRef(newMethod.Parameters[paramIndex], paramIndex + argOffset, paramVariable,
                                imports);

                            bodyBuilder.Add(OpCodes.Br_S, continueBlock);

                            // There is no need to handle generic value types, they are already passed by reference

                            continueBlock.Instruction = bodyBuilder.Add(OpCodes.Nop);
                        }
                        else
                        {
                            bodyBuilder.EmitUpdateRef(newMethod.Parameters[paramIndex], paramIndex + argOffset, paramVariable,
                                imports);
                        }
                    }

                    bodyBuilder.EmitPointerToObject(originalMethod.Signature!.ReturnType, newMethod.Signature.ReturnType, typeContext,
                        resultVar, false, true);

                    bodyBuilder.Add(OpCodes.Ret);
                }
            }
        }
    }

    // Emits a direct native call for parameterless methods returning a multi-field blittable struct,
    // bypassing il2cpp_runtime_invoke (whose arm64 boxing corrupts HFA returns to (x,x,x)). Returns
    // true if it emitted the whole body (caller should skip the normal runtime_invoke generation),
    // false to fall through to the normal path.
    private static bool TryEmitDirectValueTypeGetter(RewriteGlobalContext context,
        CilInstructionCollection bodyBuilder, MethodRewriteContext methodRewriteContext,
        TypeRewriteContext typeContext, RuntimeAssemblyReferences imports)
    {
        var originalMethod = methodRewriteContext.OriginalMethod;
        var newMethod = methodRewriteContext.NewMethod;

        // Parameterless only (no args to marshal), and never for the generic-instantiation store path.
        if (originalMethod.Parameters.Count != 0 || methodRewriteContext.GenericInstantiationsStoreSelfSubstRef != null)
            return false;

        // Skip anything that needs virtual dispatch - a direct pointer call would ignore overrides.
        // (Matches the condition the normal path uses to fetch the virtual method pointer.)
        if (originalMethod.IsAbstract || (originalMethod.IsVirtual && !originalMethod.DeclaringType!.IsValueType))
            return false;

        var originalReturnType = originalMethod.Signature!.ReturnType;
        if (!originalReturnType.IsValueType)
            return false;

        var returnTypeDef = originalReturnType.Resolve();
        if (returnTypeDef == null)
            return false;

        var returnCtx = context.TryGetNewTypeForOriginal(returnTypeDef);
        if (returnCtx == null || returnCtx.ComputedTypeSpecifics != TypeRewriteContext.TypeSpecifics.BlittableStruct)
            return false;

        // Only multi-field structs are affected (Vector2/3/4, Quaternion, Color, ...); single-value
        // returns (primitives via 1 field, enums) don't corrupt, so leave them on the safe path.
        var instanceFieldCount = 0;
        foreach (var field in returnTypeDef.Fields)
            if (!field.IsStatic)
                instanceFieldCount++;
        if (instanceFieldCount < 2)
            return false;

        var convertedReturnType = newMethod.Signature!.ReturnType;

        if (originalMethod.IsStatic)
        {
            // T CallValueTypeGetterStatic<T>(IntPtr methodInfo)
            bodyBuilder.Add(OpCodes.Ldsfld, methodRewriteContext.NonGenericMethodInfoPointerField);
            bodyBuilder.Add(OpCodes.Call, imports.Module.DefaultImporter.ImportMethod(
                imports.IL2CPP_CallValueTypeGetterStatic.Value.MakeGenericInstanceMethod(convertedReturnType)));
        }
        else
        {
            // T CallValueTypeGetterInstance<T>(IntPtr methodInfo, IntPtr obj)
            bodyBuilder.Add(OpCodes.Ldsfld, methodRewriteContext.NonGenericMethodInfoPointerField);
            bodyBuilder.EmitObjectToPointer(originalMethod.DeclaringType!.ToTypeSignature(),
                newMethod.DeclaringType!.ToTypeSignature(), typeContext, 0, true, false, true, true, out _);
            bodyBuilder.Add(OpCodes.Call, imports.Module.DefaultImporter.ImportMethod(
                imports.IL2CPP_CallValueTypeGetterInstance.Value.MakeGenericInstanceMethod(convertedReturnType)));
        }

        bodyBuilder.Add(OpCodes.Ret);
        return true;
    }
}
