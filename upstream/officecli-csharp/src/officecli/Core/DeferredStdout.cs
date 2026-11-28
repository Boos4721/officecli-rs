// Copyright 2026 OfficeCLI (https://OfficeCLI.AI)
// SPDX-License-Identifier: Apache-2.0

namespace OfficeCli.Core;

/// <summary>
/// Holds a mutating command's stdout until the document has been saved.
///
/// A non-resident add/set/remove/… writes its success line while the handler
/// is still open; the actual save happens when the handler is disposed at the
/// end of the command. When that save fails (#493) the command must not have
/// already printed "Updated …" — in --json mode that produced a
/// {"success":true} envelope followed by a {"success":false} one on the same
/// stdout. Declare it right AFTER the handler so it is disposed FIRST:
/// <code>
///   using var handler = DocumentHandlerFactory.Open(path, editable: true);
///   using var deferred = DeferredStdout.Begin(handler);
/// </code>
/// Its Dispose saves (disposes) the handler, and only when that succeeds
/// releases the buffered output; on failure the output is dropped and the
/// save error propagates to the command's error envelope. The handler's own
/// `using` then disposes it a second time, which is a no-op. stderr is not
/// buffered.
/// </summary>
public sealed class DeferredStdout : IDisposable
{
    private readonly IDisposable _handler;
    private readonly TextWriter _original;
    private readonly StringWriter _buffer = new();
    private bool _done;

    private DeferredStdout(IDisposable handler)
    {
        _handler = handler;
        _original = Console.Out;
        Console.SetOut(_buffer);
    }

    public static DeferredStdout Begin(IDisposable handler) => new(handler);

    public void Dispose()
    {
        if (_done) return;
        _done = true;
        Console.SetOut(_original);
        _handler.Dispose(); // throws on a failed save — the buffered success output is dropped
        _original.Write(_buffer.ToString());
        _original.Flush();
    }
}
