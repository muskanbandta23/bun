use super::not_supported;

not_supported! {
    NewPromise CreatePromise GetDisposeMethod NewSuppressedError AsyncIteratorOpen AsyncIteratorNext
    ResumeDispatch CreateGenerator CreateAsyncGenerator Suspend Await Resume ResumeAfterYield ResumeAfterAwait
    ResumeAfterDelegate GeneratorReturn GeneratorThrow AsyncReturn AsyncThrow DebugAwait
}
