// Lazy invoke — window.__TAURI__ is injected by Tauri after DOM load;
// calling it lazily (inside function body) avoids module-init crash.
function invoke(cmd, args) {
  return window.__TAURI__.core.invoke(cmd, args);
}

// Milestone 2+ — real IPC calls
export const listConnections   = ()         => invoke("list_connections",   {});
export const createConnection  = (req)      => invoke("create_connection",  { req });
export const updateConnection  = (id, req)  => invoke("update_connection",  { id, req });
export const deleteConnection  = (id)       => invoke("delete_connection",  { id });
export const startServer       = (id)       => invoke("start_server",       { id });
export const stopServer        = (id)       => invoke("stop_server",        { id });
export const getGlobalPort     = ()         => invoke("get_global_port",    {});
export const setGlobalPort     = (port)     => invoke("set_global_port",    { port });
export const pickFolder        = ()         => invoke("pick_folder");
export const getAuditLog       = (limit)    => invoke("get_audit_log",      { limit: limit ?? 50 });
export const startOAuthFlow    = (id)       => invoke("start_oauth_flow",   { connectionId: id });
