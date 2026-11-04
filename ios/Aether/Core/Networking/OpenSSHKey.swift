import Crypto
import Foundation

/// The OpenSSH private key container ("-----BEGIN OPENSSH PRIVATE KEY-----", unencrypted) and the
/// `ssh-keygen -l` fingerprint form, so a pairing payload's key and host key can be used with NIOSSH.
enum OpenSSHKey {
    enum ParseError: Error, Equatable { case notPEM, notOpenSSH, encrypted, notEd25519, malformed }

    /// The ed25519 private key of an unencrypted OpenSSH PEM.
    static func ed25519PrivateKey(pem: String) throws -> Curve25519.Signing.PrivateKey {
        let lines = pem.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }
        guard let begin = lines.firstIndex(where: { $0.hasPrefix("-----BEGIN OPENSSH PRIVATE KEY-----") }) else {
            // Other PEM kinds (PKCS#8 "BEGIN PRIVATE KEY", RSA) are not what pairing produces.
            throw lines.contains(where: { $0.hasPrefix("-----BEGIN") }) ? ParseError.notOpenSSH : ParseError.notPEM
        }
        let end = lines.firstIndex(where: { $0.hasPrefix("-----END OPENSSH PRIVATE KEY-----") }) ?? lines.count
        let body = lines[(begin + 1)..<end].joined()
        guard let data = Data(base64Encoded: body) else { throw ParseError.malformed }
        var r = Reader(data)
        guard r.take(15) == Data("openssh-key-v1\0".utf8) else { throw ParseError.malformed }
        guard let cipher = r.string(), let kdf = r.string(), r.bytes() != nil, let count = r.uint32() else { throw ParseError.malformed }
        guard cipher == "none", kdf == "none" else { throw ParseError.encrypted }
        guard count == 1, r.bytes() != nil, let priv = r.bytes() else { throw ParseError.malformed }
        var p = Reader(priv)
        guard let check1 = p.uint32(), let check2 = p.uint32(), check1 == check2 else { throw ParseError.malformed }
        guard let type = p.string() else { throw ParseError.malformed }
        guard type == "ssh-ed25519" else { throw ParseError.notEd25519 }
        guard p.bytes() != nil, let secret = p.bytes(), secret.count == 64 else { throw ParseError.malformed }
        return try Curve25519.Signing.PrivateKey(rawRepresentation: secret.prefix(32))
    }

    /// "SHA256:<base64 without padding>" of a public key's wire blob, like `ssh-keygen -l`.
    static func fingerprint(wireBlob: Data) -> String {
        let digest = SHA256.hash(data: wireBlob)
        return "SHA256:" + Data(digest).base64EncodedString().trimmingCharacters(in: CharacterSet(charactersIn: "="))
    }

    /// The wire blob of an OpenSSH public key line ("ssh-ed25519 AAAA… comment").
    static func wireBlob(openSSHPublicKey line: String) -> Data? {
        let parts = line.split(separator: " ")
        guard parts.count >= 2 else { return nil }
        return Data(base64Encoded: String(parts[1]))
    }

    /// The OpenSSH public key line of an ed25519 key: "ssh-ed25519 <base64 blob> <comment>".
    static func publicKeyLine(_ key: Curve25519.Signing.PrivateKey, comment: String) -> String {
        var blob = Data()
        blob.appendSSHString(Data("ssh-ed25519".utf8))
        blob.appendSSHString(key.publicKey.rawRepresentation)
        return "ssh-ed25519 \(blob.base64EncodedString()) \(comment)"
    }

    /// An unencrypted OpenSSH PEM of an ed25519 key (what `ssh-keygen -t ed25519 -N ''` writes).
    static func pem(_ key: Curve25519.Signing.PrivateKey, comment: String) -> String {
        var pub = Data()
        pub.appendSSHString(Data("ssh-ed25519".utf8))
        pub.appendSSHString(key.publicKey.rawRepresentation)
        var priv = Data()
        let check = UInt32.random(in: 0...UInt32.max)
        priv.appendUInt32(check)
        priv.appendUInt32(check)
        priv.appendSSHString(Data("ssh-ed25519".utf8))
        priv.appendSSHString(key.publicKey.rawRepresentation)
        priv.appendSSHString(key.rawRepresentation + key.publicKey.rawRepresentation)
        priv.appendSSHString(Data(comment.utf8))
        var pad: UInt8 = 1
        while priv.count % 8 != 0 { priv.append(pad); pad += 1 }
        var out = Data("openssh-key-v1\0".utf8)
        out.appendSSHString(Data("none".utf8))
        out.appendSSHString(Data("none".utf8))
        out.appendSSHString(Data())
        out.appendUInt32(1)
        out.appendSSHString(pub)
        out.appendSSHString(priv)
        let b64 = out.base64EncodedString()
        var lines: [String] = ["-----BEGIN OPENSSH PRIVATE KEY-----"]
        var i = b64.startIndex
        while i < b64.endIndex {
            let j = b64.index(i, offsetBy: 70, limitedBy: b64.endIndex) ?? b64.endIndex
            lines.append(String(b64[i..<j]))
            i = j
        }
        lines.append("-----END OPENSSH PRIVATE KEY-----")
        return lines.joined(separator: "\n") + "\n"
    }

    private struct Reader {
        let data: Data
        var offset: Int
        init(_ data: Data) { self.data = data; offset = data.startIndex }

        mutating func take(_ n: Int) -> Data? {
            guard n >= 0, data.endIndex - offset >= n else { return nil }
            let out = data[offset..<(offset + n)]
            offset += n
            return Data(out)
        }

        mutating func uint32() -> UInt32? {
            guard let b = take(4) else { return nil }
            return b.reduce(0) { ($0 << 8) | UInt32($1) }
        }

        mutating func bytes() -> Data? {
            guard let n = uint32() else { return nil }
            return take(Int(n))
        }

        mutating func string() -> String? {
            bytes().map { String(decoding: $0, as: UTF8.self) }
        }
    }
}

extension Data {
    mutating func appendUInt32(_ v: UInt32) {
        append(contentsOf: [UInt8(v >> 24), UInt8((v >> 16) & 0xFF), UInt8((v >> 8) & 0xFF), UInt8(v & 0xFF)])
    }

    mutating func appendSSHString(_ d: Data) {
        appendUInt32(UInt32(d.count))
        append(d)
    }
}
