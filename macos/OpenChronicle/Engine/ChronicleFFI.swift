import Foundation

enum ChronicleFFI {
    private static let okStatus = UInt32(CHRONICLE_OK)

    static func schemaIdentity() throws -> ChronicleSchemaIdentity {
        var output = ChronicleBuffer(token: 0, ptr: nil, len: 0)
        let status = chronicle_schema_version(&output)
        let data = try consume(&output)
        try requireSuccess(status: status, response: data)
        return try decodeEnvelope(ChronicleSchemaIdentity.self, from: data)
            .requireCompatibleMajor()
    }

    static func open(request: Data) throws -> (UInt64, ChronicleOpenResult) {
        var handle: UInt64 = 0
        let (status, data) = try request.withUnsafeBytes { bytes in
            var output = ChronicleBuffer(token: 0, ptr: nil, len: 0)
            let status = chronicle_open(
                bytes.bindMemory(to: UInt8.self).baseAddress,
                bytes.count,
                &handle,
                &output
            )
            return (status, try consume(&output))
        }
        try requireSuccess(status: status, response: data)
        let opened = try decodeEnvelope(ChronicleOpenResult.self, from: data)
            .requireCompatibleMajor()
        guard handle != 0 else { throw ChronicleBridgeError.malformedResponse }
        return (handle, opened)
    }

    static func call(handle: UInt64, request: Data) throws -> Data {
        try invokeJSON(request: request) { pointer, count, output in
            chronicle_call(handle, pointer, count, output)
        }
    }

    static func ingest(handle: UInt64, request: Data, image: Data?) throws -> Data {
        try request.withUnsafeBytes { requestBytes in
            if let image {
                return try image.withUnsafeBytes { imageBytes in
                    try invokeJSON(
                        requestPointer: requestBytes.bindMemory(to: UInt8.self).baseAddress,
                        requestCount: requestBytes.count
                    ) { output in
                        chronicle_ingest(
                            handle,
                            requestBytes.bindMemory(to: UInt8.self).baseAddress,
                            requestBytes.count,
                            imageBytes.bindMemory(to: UInt8.self).baseAddress,
                            imageBytes.count,
                            output
                        )
                    }
                }
            }
            return try invokeJSON(
                requestPointer: requestBytes.bindMemory(to: UInt8.self).baseAddress,
                requestCount: requestBytes.count
            ) { output in
                chronicle_ingest(
                    handle,
                    requestBytes.bindMemory(to: UInt8.self).baseAddress,
                    requestBytes.count,
                    nil,
                    0,
                    output
                )
            }
        }
    }

    static func imageRead(handle: UInt64, request: Data) throws -> Data {
        try request.withUnsafeBytes { bytes in
            var output = ChronicleBuffer(token: 0, ptr: nil, len: 0)
            let status = chronicle_image_read(
                handle,
                bytes.bindMemory(to: UInt8.self).baseAddress,
                bytes.count,
                &output
            )
            let copied = try consume(&output)
            // Image success is raw bytes; errors are versioned JSON. Branch on
            // status before attempting any envelope decoding.
            try requireSuccess(status: status, response: copied)
            return copied
        }
    }

    static func close(handle: UInt64) throws {
        var output = ChronicleBuffer(token: 0, ptr: nil, len: 0)
        let status = chronicle_close(handle, &output)
        let data = try consume(&output)
        try requireSuccess(status: status, response: data)
    }

    static func decodeEnvelope<Result: Codable & Sendable>(
        _ type: Result.Type,
        from data: Data
    ) throws -> ChronicleEnvelope<Result> {
        do {
            return try JSONDecoder().decode(ChronicleEnvelope<Result>.self, from: data)
        } catch {
            throw ChronicleBridgeError.malformedResponse
        }
    }

    private static func invokeJSON(
        request: Data,
        operation: (
            UnsafePointer<UInt8>?,
            Int,
            UnsafeMutablePointer<ChronicleBuffer>?
        ) -> UInt32
    ) throws -> Data {
        try request.withUnsafeBytes { bytes in
            try invokeJSON(
                requestPointer: bytes.bindMemory(to: UInt8.self).baseAddress,
                requestCount: bytes.count
            ) { output in
                operation(
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    output
                )
            }
        }
    }

    private static func invokeJSON(
        requestPointer: UnsafePointer<UInt8>?,
        requestCount: Int,
        operation: (UnsafeMutablePointer<ChronicleBuffer>?) -> UInt32
    ) throws -> Data {
        _ = requestPointer
        _ = requestCount
        var output = ChronicleBuffer(token: 0, ptr: nil, len: 0)
        let status = operation(&output)
        let data = try consume(&output)
        try requireSuccess(status: status, response: data)
        return data
    }

    private static func consume(_ output: inout ChronicleBuffer) throws -> Data {
        guard output.token != 0, let pointer = output.ptr, output.len > 0 else {
            throw ChronicleBridgeError.malformedResponse
        }
        // Copy before freeing, including on JSON decode failures in callers.
        let data = Data(bytes: pointer, count: output.len)
        guard chronicle_buffer_free(&output) == okStatus else {
            throw ChronicleBridgeError.malformedResponse
        }
        return data
    }

    private static func requireSuccess(status: UInt32, response: Data) throws {
        guard status == okStatus else {
            let payload = try? JSONDecoder().decode(
                ChronicleEnvelope<ChronicleSchemaIdentity>.self,
                from: response
            ).error
            throw ChronicleBridgeError.bridgeStatus(status, payload)
        }
    }
}
