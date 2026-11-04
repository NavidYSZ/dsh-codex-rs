import SwiftUI
import UIKit

/// Images of the chat (`/api/attachments/:id`), cached per attachment id; `<img>` cannot send the bearer, the client can.
@MainActor
final class ImageCache {
    static let shared = ImageCache()
    private let cache = NSCache<NSString, UIImage>()
    private var inflight: [String: Task<UIImage?, Never>] = [:]

    func image(for ref: ImageAttachmentRef, client: any ServerAPI) async -> UIImage? {
        let id = ref.attachmentId
        guard !id.isEmpty else { return nil }
        if let cached = cache.object(forKey: id as NSString) { return cached }
        if let task = inflight[id] { return await task.value }
        let task = Task<UIImage?, Never> {
            guard let data = try? await client.image(ref), let image = UIImage(data: data) else { return nil }
            return image
        }
        inflight[id] = task
        let image = await task.value
        inflight[id] = nil
        if let image { cache.setObject(image, forKey: id as NSString) }
        return image
    }
}

struct AttachmentImage: View {
    let ref: ImageAttachmentRef
    let client: any ServerAPI
    @State private var image: UIImage?
    @State private var failed = false

    var body: some View {
        ZStack {
            if let image {
                Image(uiImage: image).resizable().scaledToFit()
            } else if failed {
                Image(systemName: "photo").foregroundStyle(Theme.textMuted)
            } else {
                ProgressView()
            }
        }
        .background(Theme.surfaceRaised)
        .task(id: ref.attachmentId) {
            image = await ImageCache.shared.image(for: ref, client: client)
            failed = image == nil
        }
        .accessibilityLabel(L.chatA11yImage)
    }
}
