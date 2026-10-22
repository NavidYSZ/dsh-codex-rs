import AppKit
import WebKit

/// `window.dshxDesktop` in every instance page. The contract is documented in desktop/README.md.
enum BridgeScript {
    static let source = #"""
    (function () {
      'use strict';
      if (window.dshxDesktop) return;
      var handler = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.dshx;
      if (!handler) return;
      var state = __DSHX_STATE__;
      var call = function (type, payload) {
        var message = { type: type };
        if (payload) for (var k in payload) message[k] = payload[k];
        return handler.postMessage(message);
      };
      var clone = function (v) { return JSON.parse(JSON.stringify(v)); };

      // Keeps today's header rows clear of the traffic lights. The web UI can
      // drop it (#dshx-desktop-defaults) once it uses the CSS variables itself.
      var DEFAULTS = 'html.dshx-desktop:not(.dshx-fullscreen) .sb-head,' +
        'html.dshx-desktop:not(.dshx-fullscreen) .topbar.is-collapsed' +
        '{padding-left:calc(var(--dshx-traffic-lights-width) + 6px)}';
      var defaultsAdded = false;

      function applyChrome() {
        var root = document.documentElement;
        if (!root) return;
        if (!defaultsAdded) {
          defaultsAdded = true;
          var style = document.createElement('style');
          style.id = 'dshx-desktop-defaults';
          style.textContent = DEFAULTS;
          (document.head || root).appendChild(style);
        }
        var bar = state.titlebar;
        root.classList.add('dshx-desktop');
        root.classList.toggle('dshx-fullscreen', !!bar.fullScreen);
        root.style.setProperty('--dshx-titlebar-height', bar.height + 'px');
        root.style.setProperty('--dshx-traffic-lights-width', (bar.fullScreen ? 0 : bar.trafficLightsWidth) + 'px');
      }
      applyChrome();
      document.addEventListener('readystatechange', applyChrome);

      var api = {
        get version() { return state.version; },
        get platform() { return 'macos'; },
        get current() { return state.current; },
        get instances() { return clone(state.instances); },
        get capabilities() { return clone(state.capabilities); },
        get titlebar() { return clone(state.titlebar); },
        switchTo: function (id) { return call('switchTo', { id: String(id) }); },
        addInstance: function () { return call('addInstance'); },
        editInstances: function () { return call('editInstances'); },
        notify: function (title, body, options) {
          var tag = options && options.tag != null ? String(options.tag) : '';
          return call('notify', { title: title == null ? '' : String(title), body: body == null ? '' : String(body), tag: tag });
        },
        setBadge: function (n) { return call('setBadge', { count: Math.max(0, Math.floor(Number(n) || 0)) }); },
        revealInFinder: function (path) { return call('revealInFinder', { path: String(path) }); },
        openInEditor: function (path) { return call('openInEditor', { path: String(path) }); },
        pickFolder: function (options) {
          var o = options || {};
          var text = function (v) { return v == null ? '' : String(v); };
          return call('pickFolder', { title: text(o.title), prompt: text(o.prompt), defaultPath: text(o.defaultPath) });
        },
        ready: function () { return call('ready'); },
        setAppearance: function (appearance) {
          var a = appearance || {};
          return call('setAppearance', { background: String(a.background || ''), theme: String(a.theme || '') });
        },
        prewarm: function (id) { return call('prewarm', { id: String(id) }); }
      };
      Object.defineProperty(window, 'dshxDesktop', { value: Object.freeze(api), enumerable: true });

      var fallbacks = {
        'new-chat': ['[data-dshx-command="new-chat"]', '.sb-nav .nav-item', '.topbar-new'],
        'settings': ['[data-dshx-command="settings"]', '.sb-foot .nav-item'],
        'open-project': ['[data-dshx-command="open-project"]']
      };
      function command(name) {
        var event = new CustomEvent('dshx-desktop-command', { detail: { command: name }, cancelable: true });
        if (!window.dispatchEvent(event)) return;
        var list = fallbacks[name] || [];
        for (var i = 0; i < list.length; i++) {
          var el = document.querySelector(list[i]);
          if (el) { el.click(); return; }
        }
      }

      Object.defineProperty(window, '__dshxDesktopReceive', {
        value: function (message) {
          if (message.state) {
            state = message.state;
            applyChrome();
            window.dispatchEvent(new CustomEvent('dshx-desktop-change', {
              detail: { current: state.current, instances: clone(state.instances) }
            }));
          }
          if (message.command) command(message.command);
          if (message.notification) {
            window.dispatchEvent(new CustomEvent('dshx-desktop-notification', { detail: clone(message.notification) }));
          }
          if (message.drag) window.dispatchEvent(new CustomEvent('dshx-desktop-drag', { detail: clone(message.drag) }));
          if (message.drop) window.dispatchEvent(new CustomEvent('dshx-desktop-drop', { detail: clone(message.drop) }));
        }
      });

      // Window dragging: the strip under the traffic lights and anything marked
      // [data-dshx-drag] moves the window, unless the click hits a control.
      var CONTROLS = 'a,button,input,textarea,select,option,label,summary,video,audio,iframe,' +
        '[role=button],[role=link],[role=tab],[role=menuitem],[role=option],[role=slider],' +
        '[contenteditable=""],[contenteditable=true],[draggable=true],[data-dshx-no-drag]';
      window.addEventListener('mousedown', function (e) {
        if (e.button !== 0 || e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
        var target = e.target instanceof Element ? e.target : null;
        var marked = target && target.closest('[data-dshx-drag]');
        if (!marked && e.clientY > state.titlebar.height) return;
        if (target && target.closest(CONTROLS)) return;
        if (e.detail === 2) { call('titlebarDoubleClick'); return; }
        if (e.detail === 1) call('drag');
      }, true);
    })();
    """#
}

/// Receives `window.webkit.messageHandlers.dshx` calls of one instance page.
@MainActor
final class BridgeHandler: NSObject, WKScriptMessageHandlerWithReply {
    let instanceID: String
    weak var controller: MainWindowController?

    init(instanceID: String, controller: MainWindowController) {
        self.instanceID = instanceID
        self.controller = controller
    }

    func userContentController(
        _ userContentController: WKUserContentController,
        didReceive message: WKScriptMessage,
        replyHandler: @escaping @MainActor @Sendable (Any?, String?) -> Void
    ) {
        guard let controller, message.frameInfo.isMainFrame,
              controller.isTrusted(origin: message.frameInfo.securityOrigin, for: instanceID),
              let body = message.body as? [String: Any], let type = body["type"] as? String
        else {
            replyHandler(nil, "not allowed")
            return
        }
        let string = { (key: String) in body[key] as? String ?? "" }
        switch type {
        case "switchTo":
            replyHandler(controller.show(string("id")), nil)
        case "addInstance":
            controller.addInstance()
            replyHandler(nil, nil)
        case "editInstances":
            controller.editInstances()
            replyHandler(nil, nil)
        case "notify":
            Notifier.shared.post(
                title: string("title"), body: string("body"), tag: string("tag"),
                instanceID: instanceID, instanceName: controller.manager.instance(instanceID)?.name ?? "dsh-codex")
            replyHandler(true, nil)
        case "setBadge":
            let count = (body["count"] as? NSNumber)?.intValue ?? 0
            controller.manager.setBadge(count, for: instanceID)
            replyHandler(nil, nil)
        case "revealInFinder":
            replyHandler(controller.revealInFinder(string("path"), from: instanceID), nil)
        case "openInEditor":
            replyHandler(controller.openInEditor(string("path"), from: instanceID), nil)
        case "pickFolder":
            controller.pickFolder(
                title: string("title"), prompt: string("prompt"), defaultPath: string("defaultPath"),
                from: instanceID) { path in replyHandler(path, nil) }
        case "ready":
            controller.pageReady(instanceID)
            replyHandler(nil, nil)
        case "setAppearance":
            controller.setAppearance(background: string("background"), theme: string("theme"), from: instanceID)
            replyHandler(nil, nil)
        case "prewarm":
            controller.manager.prewarm(string("id"))
            replyHandler(nil, nil)
        case "drag":
            controller.beginWindowDrag()
            replyHandler(nil, nil)
        case "titlebarDoubleClick":
            controller.titlebarDoubleClick()
            replyHandler(nil, nil)
        default:
            replyHandler(nil, "unknown call \(type)")
        }
    }
}
