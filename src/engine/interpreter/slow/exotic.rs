use super::not_supported;

not_supported! {
    CreateDirectArguments CreateScopedArguments CreateUnmappedArguments GetFromArguments PutToArguments
    EnsureArguments CopyDataProperties CloneObject CopyRest
    GetPrivateName PutPrivateName DefinePrivateName HasPrivateName SetPrivateBrand CheckPrivateBrand HasPrivateBrand
    ForInOpen ForInNext ForInGet ForInPut ForInHas ForInHasOwn
}
