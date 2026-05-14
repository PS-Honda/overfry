const { invoke } = window.__TAURI__.core;

export const greet = (name) => invoke("greet", { name });

// Milestone 2+ — stubs so dashboard.js can import without error
export const listConnections  = ()       => Promise.resolve([]);
export const createConnection = (req)    => invoke("create_connection", { req });
export const deleteConnection = (id)     => invoke("delete_connection", { id });
export const startServer      = (id)     => invoke("start_server",      { id });
export const stopServer       = (id)     => invoke("stop_server",       { id });
export const suggestPort      = (port)   => invoke("suggest_port",      { requested: port ?? null });
export const pickFolder       = ()       => invoke("pick_folder");
export const getAuditLog      = (limit)  => invoke("get_audit_log",     { limit: limit ?? 50 });
