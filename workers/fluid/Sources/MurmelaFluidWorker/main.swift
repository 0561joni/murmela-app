// murmela-fluid-worker: resident live-pair helper. Requests are handled one at
// a time, in order; each request receives exactly one response.
import Foundation

let engine = Engine()

/// Session-level failures are answered and the worker stays resident. Fatal
/// (framing) failures are answered and the worker exits nonzero. Error
/// responses carry only a code; details go to stderr and never contain text.
@MainActor
func fail(_ error: WorkerError, session: String, sequence: Int) {
    Output.send("error", session: session, sequence: sequence + 1, code: error.code)
    FileHandle.standardError.write(Data("\(error.code)\n".utf8))
    if ProcessInfo.processInfo.environment["MURMELA_FLUID_PROBE"] != nil {
        FileHandle.standardError.write(Data("\(error.detail)\n".utf8))
    }
    if error.fatal { exit(1) }
}

@MainActor
func handle(_ request: Request) async throws {
    let reply: (String, [String: Any])
    switch request.type {
    case "load":
        reply = (
            "ready",
            try await engine.load(
                nemotronPath: try request.string("nemotronPath"),
                parakeetPath: try request.string("parakeetPath"))
        )
    case "warmup": reply = ("warm", try await engine.warmup())
    case "ping": reply = ("pong", engine.ping())
    case "unload":
        await engine.unload()
        reply = ("unloaded", [:])
    case "start": reply = ("started", try await engine.start(request))
    case "audio": reply = ("progress", try await engine.audio(request))
    case "finish": reply = ("final", try await engine.finish(request))
    case "cancel", "reset":
        await engine.cancel(request)
        reply = ("canceled", [:])
    default: throw WorkerError.protocol("unknown request")
    }
    Output.send(reply.0, session: request.sessionId, sequence: request.sequence + 1, reply.1)
}

@MainActor
func run() async -> Int32 {
    for await event in startInputThread() {
        switch event {
        case .line(let line):
            if line.isEmpty { continue }
            let request: Request
            do {
                request = try Request(line: line)
            } catch let error as WorkerError {
                fail(error, session: "", sequence: 0)
                return 1
            } catch {
                return 1
            }
            do {
                try await handle(request)
            } catch let error as WorkerError {
                fail(error, session: request.sessionId, sequence: request.sequence)
            } catch {
                fail(
                    WorkerError.session("FLUID_RUNTIME_ERROR", "\(error)"), session: request.sessionId,
                    sequence: request.sequence)
            }
        case .oversized:
            fail(.protocol("frame too large"), session: "", sequence: 0)
            return 1
        case .eof(let partial):
            // Closing stdin between sessions is a clean shutdown.
            if partial {
                fail(.protocol("truncated frame"), session: "", sequence: 0)
                return 1
            }
            return engine.hasActiveSession ? 1 : 0
        }
    }
    return 0
}

let status = await run()
exit(status)
