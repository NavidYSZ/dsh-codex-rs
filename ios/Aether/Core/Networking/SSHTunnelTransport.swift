import Crypto
import Foundation
import NIOConcurrencyHelpers
import NIOCore
import NIOPosix
import NIOSSH

/// The `ssh` transport (SPEC §2): an SSH connection with ed25519 user auth and a pinned host key, a local
/// listener on 127.0.0.1:0 whose accepted connections are piped into `direct-tcpip` channels to
/// `remoteHost:remotePort`, and `tokenCommand` over an exec channel when the payload carries no token.
/// URLSession then talks plain HTTP to `http://127.0.0.1:<local>`, which the server's loopback check accepts.
final class SSHTunnelTransport: Transport, @unchecked Sendable {
    let kind: TransportKind = .ssh
    private let options: StoredTransport.SSHOptions
    private let privateKeyPEM: String
    private let token: String?
    private let cachedCommandToken: String?
    private let persistToken: @Sendable (String) -> Void
    private let connectTimeout: TimeInterval

    private let state = NIOLockedValueBox<State>(State())

    private struct State {
        var group: MultiThreadedEventLoopGroup?
        var ssh: Channel?
        var listener: Channel?
        var commandToken: String?
    }

    init(options: StoredTransport.SSHOptions, privateKeyPEM: String, token: String?, cachedCommandToken: String? = nil,
         connectTimeout: TimeInterval = 15, persistToken: @escaping @Sendable (String) -> Void = { _ in }) {
        self.options = options
        self.privateKeyPEM = privateKeyPEM
        self.token = token
        self.cachedCommandToken = cachedCommandToken
        self.connectTimeout = connectTimeout
        self.persistToken = persistToken
        state.withLockedValue { $0.commandToken = cachedCommandToken }
    }

    deinit {
        let s = state.withLockedValue { $0 }
        s.listener?.close(promise: nil)
        s.ssh?.close(promise: nil)
        s.group?.shutdownGracefully { _ in }
    }

    // MARK: Transport

    func open() async throws -> Endpoint {
        let key: Curve25519.Signing.PrivateKey
        do { key = try OpenSSHKey.ed25519PrivateKey(pem: privateKeyPEM) } catch { throw ApiError.badKey }
        let group = MultiThreadedEventLoopGroup(numberOfThreads: 1)
        state.withLockedValue { $0.group = group }
        let errors = ErrorBox()
        let auth = KeyAuthDelegate(username: options.user, key: key)
        let hostKeys = PinnedHostKeyDelegate(expected: options.hostKeySha256, errors: errors)
        let bootstrap = ClientBootstrap(group: group)
            .connectTimeout(.seconds(Int64(connectTimeout)))
            .channelOption(ChannelOptions.socketOption(.so_reuseaddr), value: 1)
            .channelOption(ChannelOptions.socketOption(.tcp_nodelay), value: 1)
            .channelInitializer { channel in
                channel.eventLoop.makeCompletedFuture {
                    let ssh = NIOSSHHandler(role: .client(.init(userAuthDelegate: auth, serverAuthDelegate: hostKeys)),
                                            allocator: channel.allocator, inboundChildChannelInitializer: nil)
                    try channel.pipeline.syncOperations.addHandler(ssh)
                    try channel.pipeline.syncOperations.addHandler(ErrorRecorder(errors: errors))
                }
            }
        let ssh: Channel
        do {
            ssh = try await bootstrap.connect(host: options.host, port: options.port).get()
        } catch {
            await close()
            throw ApiError.sshConnect(host: options.host, String(describing: error))
        }
        state.withLockedValue { $0.ssh = ssh }

        // A probe channel: proves the key was accepted and the forwarding target is allowed, before anything listens.
        do {
            let probe = try await Self.directChannel(ssh: ssh, options: options) { child in
                child.eventLoop.makeCompletedFuture {
                    try child.pipeline.syncOperations.addHandler(SSHWrapperHandler())
                }
            }.get()
            try? await probe.close().get()
        } catch {
            let reason = errors.first ?? error
            await close()
            throw Self.map(reason, host: options.host)
        }

        // The local end: every accepted connection becomes a direct-tcpip channel glued to it.
        let options = options
        let listener: Channel
        do {
            listener = try await ServerBootstrap(group: group, childGroup: group)
                .serverChannelOption(ChannelOptions.socketOption(.so_reuseaddr), value: 1)
                .childChannelOption(ChannelOptions.socketOption(.tcp_nodelay), value: 1)
                .childChannelInitializer { inbound in
                    Self.directChannel(ssh: ssh, options: options) { child in
                        child.eventLoop.makeCompletedFuture {
                            let (ours, theirs) = GlueHandler.matchedPair()
                            try child.pipeline.syncOperations.addHandler(SSHWrapperHandler())
                            try child.pipeline.syncOperations.addHandler(ours)
                            try inbound.pipeline.syncOperations.addHandler(theirs)
                        }
                    }.map { _ in }
                }
                .bind(host: "127.0.0.1", port: 0).get()
        } catch {
            await close()
            throw ApiError.sshConnect(host: options.host, String(describing: error))
        }
        state.withLockedValue { $0.listener = listener }
        guard let port = listener.localAddress?.port, let base = URL(string: "http://127.0.0.1:\(port)") else {
            await close()
            throw ApiError.sshConnect(host: options.host, "no local port")
        }
        let bearer: any BearerSource = token.map { FixedBearer(token: $0) } ?? CommandBearer(transport: self)
        let session = DirectTransport.makeSession(certSha256: nil)
        return Endpoint(kind: .ssh, baseURL: base, session: session, bearer: bearer, label: "\(options.user)@\(options.host)")
    }

    func close() async {
        let s = state.withLockedValue { s -> State in
            let copy = s
            s.listener = nil
            s.ssh = nil
            s.group = nil
            return copy
        }
        if let l = s.listener { try? await l.close().get() }
        if let c = s.ssh { try? await c.close().get() }
        if let g = s.group { try? await g.shutdownGracefully() }
    }

    // MARK: Channels

    private static func directChannel(ssh: Channel, options: StoredTransport.SSHOptions,
                                      _ initializer: @escaping @Sendable (Channel) -> EventLoopFuture<Void>) -> EventLoopFuture<Channel> {
        ssh.pipeline.handler(type: NIOSSHHandler.self).flatMap { handler in
            let promise = ssh.eventLoop.makePromise(of: Channel.self)
            let originator = try? SocketAddress(ipAddress: "127.0.0.1", port: 0)
            let target = SSHChannelType.DirectTCPIP(targetHost: options.remoteHost, targetPort: options.remotePort,
                                                    originatorAddress: originator ?? ssh.localAddress ?? (try! SocketAddress(ipAddress: "127.0.0.1", port: 0)))
            handler.createChannel(promise, channelType: .directTCPIP(target)) { child, type in
                guard case .directTCPIP = type else { return child.eventLoop.makeFailedFuture(TunnelError.unexpectedChannelType) }
                return initializer(child)
            }
            return promise.futureResult
        }
    }

    /// Runs `tokenCommand` over an exec channel; its stdout is the bare token or the start link, last matching line wins.
    func runTokenCommand() async throws -> String {
        guard let command = options.tokenCommand, !command.isEmpty else { throw ApiError.noToken }
        guard let ssh = state.withLockedValue({ $0.ssh }) else { throw ApiError.sshClosed }
        let output: String = try await ssh.pipeline.handler(type: NIOSSHHandler.self).flatMap { handler in
            let done = ssh.eventLoop.makePromise(of: String.self)
            let promise = ssh.eventLoop.makePromise(of: Channel.self)
            handler.createChannel(promise, channelType: .session) { child, _ in
                child.eventLoop.makeCompletedFuture {
                    try child.pipeline.syncOperations.addHandler(ExecCollector(command: command, done: done))
                }
            }
            promise.futureResult.whenFailure { done.fail($0) }
            return done.futureResult
        }.get()
        guard let token = Self.extractToken(output) else { throw ApiError.noToken }
        state.withLockedValue { $0.commandToken = token }
        persistToken(token)
        return token
    }

    var currentCommandToken: String? { state.withLockedValue { $0.commandToken } }

    /// The last line that is a start link (`…?token=<hex>`) or a bare token.
    static func extractToken(_ output: String) -> String? {
        for line in output.split(whereSeparator: \.isNewline).reversed() {
            let s = line.trimmingCharacters(in: .whitespaces)
            if let r = s.range(of: #"token=([0-9A-Za-z_\-]+)"#, options: .regularExpression) {
                return String(s[r].dropFirst(6))
            }
            if !s.isEmpty, !s.contains(" "), s.range(of: #"^[0-9A-Za-z_\-]{16,}$"#, options: .regularExpression) != nil { return s }
        }
        return nil
    }

    private static func map(_ error: Error, host: String) -> ApiError {
        if let e = error as? ApiError { return e }
        if error is AuthExhausted { return .sshAuthFailed }
        if let e = error as? NIOSSHError, "\(e)".contains("userAuth") { return .sshAuthFailed }
        return .sshConnect(host: host, String(describing: error))
    }

    enum TunnelError: Error { case unexpectedChannelType }
}

/// Bearer from `tokenCommand`: cached until a 401 asks for a fresh one.
private struct CommandBearer: BearerSource {
    let transport: SSHTunnelTransport

    func current() async throws -> String? {
        if let t = transport.currentCommandToken { return t }
        return try await transport.runTokenCommand()
    }

    func refresh() async throws -> String? {
        try await transport.runTokenCommand()
    }
}

// MARK: - Delegates and handlers

struct AuthExhausted: Error {}

/// Offers the ed25519 key once; when the server rejects it, gives up (no password, nothing interactive).
final class KeyAuthDelegate: NIOSSHClientUserAuthenticationDelegate, Sendable {
    private let username: String
    private let key: Curve25519.Signing.PrivateKey
    private let offered = NIOLockedValueBox(false)

    init(username: String, key: Curve25519.Signing.PrivateKey) {
        self.username = username
        self.key = key
    }

    func nextAuthenticationType(availableMethods: NIOSSHAvailableUserAuthenticationMethods,
                                nextChallengePromise: EventLoopPromise<NIOSSHUserAuthenticationOffer?>) {
        let first = offered.withLockedValue { done -> Bool in
            if done { return false }
            done = true
            return true
        }
        guard first, availableMethods.contains(.publicKey) else {
            nextChallengePromise.fail(AuthExhausted())
            return
        }
        let offer = NIOSSHUserAuthenticationOffer(username: username, serviceName: "",
                                                  offer: .privateKey(.init(privateKey: NIOSSHPrivateKey(ed25519Key: key))))
        nextChallengePromise.succeed(offer)
    }
}

/// Accepts the host key only when its `SHA256:` fingerprint equals the pinned one.
final class PinnedHostKeyDelegate: NIOSSHClientServerAuthenticationDelegate, Sendable {
    private let expected: String
    private let errors: ErrorBox

    init(expected: String, errors: ErrorBox) {
        self.expected = expected.trimmingCharacters(in: .whitespaces)
        self.errors = errors
    }

    func validateHostKey(hostKey: NIOSSHPublicKey, validationCompletePromise: EventLoopPromise<Void>) {
        let line = String(openSSHPublicKey: hostKey)
        guard let blob = OpenSSHKey.wireBlob(openSSHPublicKey: line) else {
            errors.record(ApiError.hostKeyMismatch)
            validationCompletePromise.fail(ApiError.hostKeyMismatch)
            return
        }
        let fingerprint = OpenSSHKey.fingerprint(wireBlob: blob)
        let wanted = expected.hasPrefix("SHA256:") ? expected : "SHA256:" + expected
        if fingerprint.trimmingCharacters(in: CharacterSet(charactersIn: "=")) == wanted.trimmingCharacters(in: CharacterSet(charactersIn: "=")) {
            validationCompletePromise.succeed(())
        } else {
            errors.record(ApiError.hostKeyMismatch)
            validationCompletePromise.fail(ApiError.hostKeyMismatch)
        }
    }
}

/// The first error the SSH pipeline saw (a failed handshake closes the channel before a child channel can tell).
final class ErrorBox: Sendable {
    private let box = NIOLockedValueBox<Error?>(nil)
    func record(_ error: Error) { box.withLockedValue { if $0 == nil { $0 = error } } }
    var first: Error? { box.withLockedValue { $0 } }
}

final class ErrorRecorder: ChannelInboundHandler {
    typealias InboundIn = Any
    private let errors: ErrorBox

    init(errors: ErrorBox) { self.errors = errors }

    func errorCaught(context: ChannelHandlerContext, error: Error) {
        errors.record(error)
        context.close(promise: nil)
    }
}

/// Unwraps `SSHChannelData` to `ByteBuffer` on the way in and wraps it on the way out.
final class SSHWrapperHandler: ChannelDuplexHandler {
    typealias InboundIn = SSHChannelData
    typealias InboundOut = ByteBuffer
    typealias OutboundIn = ByteBuffer
    typealias OutboundOut = SSHChannelData

    func channelRead(context: ChannelHandlerContext, data: NIOAny) {
        let data = unwrapInboundIn(data)
        guard case .channel = data.type, case .byteBuffer(let buffer) = data.data else { return }
        context.fireChannelRead(wrapInboundOut(buffer))
    }

    func write(context: ChannelHandlerContext, data: NIOAny, promise: EventLoopPromise<Void>?) {
        let buffer = unwrapOutboundIn(data)
        context.write(wrapOutboundOut(SSHChannelData(type: .channel, data: .byteBuffer(buffer))), promise: promise)
    }
}

/// Pipes two channels on the same event loop into each other (NIOSSH's example `GlueHandler`).
final class GlueHandler: ChannelDuplexHandler {
    typealias InboundIn = NIOAny
    typealias OutboundIn = NIOAny
    typealias OutboundOut = NIOAny

    private var partner: GlueHandler?
    private var context: ChannelHandlerContext?
    private var pendingRead = false

    private init() {}

    static func matchedPair() -> (GlueHandler, GlueHandler) {
        let a = GlueHandler(), b = GlueHandler()
        a.partner = b
        b.partner = a
        return (a, b)
    }

    private func partnerWrite(_ data: NIOAny) { context?.write(data, promise: nil) }
    private func partnerFlush() { context?.flush() }
    private func partnerWriteEOF() { context?.close(mode: .output, promise: nil) }
    private func partnerCloseFull() { context?.close(promise: nil) }
    private func partnerBecameWritable() {
        if pendingRead {
            pendingRead = false
            context?.read()
        }
    }
    private var partnerWritable: Bool { context?.channel.isWritable ?? false }

    func handlerAdded(context: ChannelHandlerContext) {
        self.context = context
        if context.channel.isWritable { partner?.partnerBecameWritable() }
    }

    func handlerRemoved(context: ChannelHandlerContext) {
        self.context = nil
        partner = nil
    }

    func channelRead(context: ChannelHandlerContext, data: NIOAny) { partner?.partnerWrite(data) }
    func channelReadComplete(context: ChannelHandlerContext) { partner?.partnerFlush() }
    func channelInactive(context: ChannelHandlerContext) { partner?.partnerCloseFull() }

    func userInboundEventTriggered(context: ChannelHandlerContext, event: Any) {
        if let event = event as? ChannelEvent, case .inputClosed = event { partner?.partnerWriteEOF() }
    }

    func errorCaught(context: ChannelHandlerContext, error: Error) { partner?.partnerCloseFull() }

    func channelWritabilityChanged(context: ChannelHandlerContext) {
        if context.channel.isWritable { partner?.partnerBecameWritable() }
    }

    func read(context: ChannelHandlerContext) {
        if let partner, partner.partnerWritable { context.read() } else { pendingRead = true }
    }
}

/// Runs one command on a session channel and collects its stdout.
final class ExecCollector: ChannelInboundHandler {
    typealias InboundIn = SSHChannelData

    private let command: String
    private var done: EventLoopPromise<String>?
    private var output = ""

    init(command: String, done: EventLoopPromise<String>) {
        self.command = command
        self.done = done
    }

    func channelActive(context: ChannelHandlerContext) {
        let exec = SSHChannelRequestEvent.ExecRequest(command: command, wantReply: true)
        // The callback runs on the channel's event loop; NIOLoopBound carries the handler and context there.
        let bound = NIOLoopBound((self, context), eventLoop: context.eventLoop)
        context.triggerUserOutboundEvent(exec).whenFailure { error in
            let (handler, context) = bound.value
            handler.done?.fail(error)
            handler.done = nil
            context.close(promise: nil)
        }
    }

    func channelRead(context: ChannelHandlerContext, data: NIOAny) {
        let data = unwrapInboundIn(data)
        guard case .byteBuffer(let buffer) = data.data else { return }
        if case .channel = data.type { output += String(buffer: buffer) }
    }

    func userInboundEventTriggered(context: ChannelHandlerContext, event: Any) {
        if event is SSHChannelRequestEvent.ExitStatus {
            finish()
            context.close(promise: nil)
        } else {
            context.fireUserInboundEventTriggered(event)
        }
    }

    func channelInactive(context: ChannelHandlerContext) { finish() }
    func errorCaught(context: ChannelHandlerContext, error: Error) {
        done?.fail(error)
        done = nil
        context.close(promise: nil)
    }

    private func finish() {
        done?.succeed(output)
        done = nil
    }
}
