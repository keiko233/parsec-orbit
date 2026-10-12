import { definePlugin } from "@parsec-orbit/sdk";

/**
 * The plugin realm. It decides what runs in the web UI and when; the work
 * itself is `ui.ts`, delivered with `injectFile`.
 */
export default definePlugin({
  async setup(host) {
    host.log.info("bandwidth ladder", host.plugin.version, "starting");

    // Capabilities are default-deny: without the manifest `permissions` the
    // property is undefined, so say what is missing instead of throwing a
    // TypeError somewhere further down.
    const { webview } = host;
    if (!webview) {
      throw new Error('this plugin needs permissions = ["webview"]');
    }

    // Messages the page sends with orbit.emit.
    webview.on("bandwidth:applied", (payload) => {
      host.log.info("the page extended the list:", payload);
    });

    // Registered at once: the bridge installs it as soon as the page exists,
    // and again whenever the page is recreated.
    const script = await webview.injectFile("dist/ui.js", { key: "ui" });
    host.log.info("ui script handed to the bridge, id", script.id);
  },
});
