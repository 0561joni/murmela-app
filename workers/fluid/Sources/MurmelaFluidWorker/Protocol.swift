// Live-pair protocol v1: UTF-8 newline-delimited JSON on stdin/stdout, the
// NDJSON framing with process-level control requests. See README.md.
import Darwin
import Foundation

let protocolVersion = 1
let maxMessageBytes = 1024 * 1024
let sampleRate = 16_000
/// Five minutes, the shortcut dictation limit.
let maxSessionSamples = 300 * sampleRate
/// Two seconds per audio frame keeps each frame well below the 1 MiB cap.
let maxFrameSamples = 2 * sampleRate

/// A failure with a stable wire code. Fatal errors mean the framing can no
/// longer be trusted: the worker reports them and exits nonzero.
struct WorkerError: Error {
    let code: String
    let fatal: Bool
    let detail: String

    static func `protocol`(_ detail: String) -> WorkerError {
        WorkerError(code: "FLUID_PROTOCOL", fatal: true, detail: detail)
    }
    static func session(_ code: String, _ detail: String) -> WorkerError {
        WorkerError(code: code, fatal: false, detail: detail)
    }
}

/// One parsed request. Accessors validate types strictly; a wrong type is a
/// protocol error, never a silent default.
struct Request {
    let type: String
    let sessionId: String
    let sequence: Int
    let fields: [String: Any]

    init(line: Data) throws {
        guard let object = try? JSONSerialization.jsonObject(with: line, options: []),
            let fields = object as? [String: Any]
        else { throw WorkerError.protocol("invalid JSON") }
        self.fields = fields
        guard Self.integer(fields["version"]) == protocolVersion else {
            throw WorkerError.protocol("unsupported version")
        }
        guard let type = fields["type"] as? String, !type.isEmpty else {
            throw WorkerError.protocol("missing type")
        }
        guard let session = fields["sessionId"] as? String, !session.isEmpty,
            session.utf8.count <= 128
        else { throw WorkerError.protocol("invalid session id") }
        guard let sequence = Self.integer(fields["sequence"]) else {
            throw WorkerError.protocol("invalid sequence")
        }
        self.type = type
        self.sessionId = session
        self.sequence = sequence
    }

    static func integer(_ value: Any?) -> Int? {
        guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else {
            return nil
        }
        let double = number.doubleValue
        guard double.isFinite, double >= 0, double <= 9_007_199_254_740_991,
            double.rounded(.towardZero) == double
        else { return nil }
        return Int(double)
    }

    func integer(_ key: String) throws -> Int {
        guard let value = Self.integer(fields[key]) else {
            throw WorkerError.protocol("expected nonnegative integer \(key)")
        }
        return value
    }

    func string(_ key: String, default fallback: String? = nil) throws -> String {
        guard let value = fields[key] else {
            if let fallback { return fallback }
            throw WorkerError.protocol("missing \(key)")
        }
        guard let string = value as? String else { throw WorkerError.protocol("expected string \(key)") }
        return string
    }

    func samples() throws -> [Float] {
        guard let values = fields["samples"] as? [Any] else {
            throw WorkerError.protocol("expected samples array")
        }
        guard !values.isEmpty, values.count <= maxFrameSamples else {
            throw WorkerError.session("FLUID_INVALID_AUDIO", "frame size out of range")
        }
        var samples = [Float]()
        samples.reserveCapacity(values.count)
        for value in values {
            guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else {
                throw WorkerError.protocol("expected numeric sample")
            }
            let sample = number.doubleValue
            guard sample.isFinite, abs(sample) <= 1 else {
                throw WorkerError.session("FLUID_INVALID_AUDIO", "sample out of range")
            }
            samples.append(Float(sample))
        }
        return samples
    }
}

func memoryBytes() -> (footprint: UInt64, peak: UInt64) {
    var info = task_vm_info_data_t()
    var count = mach_msg_type_number_t(
        MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size)
    let status = withUnsafeMutablePointer(to: &info) {
        $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
            task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
        }
    }
    var usage = rusage()
    getrusage(RUSAGE_SELF, &usage)
    return (status == KERN_SUCCESS ? info.phys_footprint : 0, UInt64(usage.ru_maxrss))
}

/// Writes exactly one response line per request. Responses never contain
/// audio and contain transcript text only in progress/final fields.
enum Output {
    static func send(
        _ type: String, session: String, sequence: Int, code: String = "",
        _ fields: [String: Any] = [:]
    ) {
        var message = fields
        let memory = memoryBytes()
        message["version"] = protocolVersion
        message["type"] = type
        message["sessionId"] = session
        message["sequence"] = sequence
        message["code"] = code
        message["rssBytes"] = memory.footprint
        message["peakRssBytes"] = memory.peak
        guard var data = try? JSONSerialization.data(withJSONObject: message, options: [.sortedKeys])
        else {
            FileHandle.standardError.write(Data("FLUID_RUNTIME_ERROR\n".utf8))
            exit(1)
        }
        data.append(0x0A)
        FileHandle.standardOutput.write(data)
    }
}

/// stdin framing, read on its own thread so the processing loop never blocks
/// on input. Lines are capped before they are buffered.
enum InputEvent: Sendable {
    case line(Data)
    case oversized
    case eof(partial: Bool)
}

func startInputThread() -> AsyncStream<InputEvent> {
    AsyncStream { continuation in
        let thread = Thread {
            var line = Data()
            var buffer = [UInt8](repeating: 0, count: 64 * 1024)
            while true {
                let count = read(STDIN_FILENO, &buffer, buffer.count)
                if count < 0 && errno == EINTR { continue }
                if count <= 0 {
                    continuation.yield(.eof(partial: !line.isEmpty))
                    continuation.finish()
                    return
                }
                var start = 0
                for index in 0..<count where buffer[index] == 0x0A {
                    line.append(contentsOf: buffer[start..<index])
                    if line.count > maxMessageBytes {
                        continuation.yield(.oversized)
                        continuation.finish()
                        return
                    }
                    continuation.yield(.line(line))
                    line = Data()
                    start = index + 1
                }
                line.append(contentsOf: buffer[start..<count])
                if line.count > maxMessageBytes {
                    continuation.yield(.oversized)
                    continuation.finish()
                    return
                }
            }
        }
        thread.stackSize = 1 << 20
        thread.start()
    }
}
