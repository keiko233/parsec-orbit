/**
 * Events of the Parsec web UI <-> client protocol that are known to work with
 * `orbit.rpc`, as far as they have been verified. The list grows as events
 * are checked against a real client.
 */

/** A `window.MTY_NativeSendText` request carries `{ protocol_version: 1, request_id, event, data? }`. */
export const PROTOCOL_VERSION = 1;

/** Reply of `cfg_get_`: the whole configuration, under `data.data`. */
export interface ConfigReply {
  data?: { data?: Record<string, unknown> };
}

/** Request data of `cfg_seti`: sets an integer option. */
export interface ConfigSetInt {
  name: string;
  value: number;
}

/** Event name to `[request data, reply]`. */
export interface UiRpc {
  cfg_get_: [undefined, ConfigReply];
  cfg_seti: [ConfigSetInt, unknown];
  appversn: [undefined, unknown];
}

/** Window event the web UI listens to for configuration changes. */
export const CONFIG_UPDATE_EVENT = "parsec_config_update";
