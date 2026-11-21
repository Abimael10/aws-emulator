// SPDX-License-Identifier: CC0-1.0

//! The JavaScript a web function's own code runs under: the HTTP server it registers, the
//! one request it is handed, and the execution environment it reads itself as.
//!
//! [`RUNTIME_SOURCE`] is a FUNCTION expression the host calls with two arguments — the
//! source of the shared module registry and the shared pure core modules
//! ([`::service_common::node_guest_modules`]) — and it answers with the protocol object the
//! host drives a request through ([`super::answer_in`]). It captures the two host functions
//! and the engine builtins it needs before any of the guest's code is compiled, so a guest
//! that rebinds `JSON`, `String` or the global object cannot change how its own response is
//! rendered or reach the host functions again.
//!
//! What is HERE is what belongs to a web function: the `http` server object model a web
//! framework registers its server through, the one request that server is handed, the timers
//! it schedules work on, the console its output is collected from, and the `process` and `os`
//! its execution environment publishes to it. Everything that is the same for any service
//! running caller-supplied Node.js code is NOT here: Node's module resolution and loading,
//! and the `path`, `util`, `events`, `querystring` and `url` modules, are compiled from the
//! one shared source this emulator keeps for them, so this runtime and the Lambda handler
//! runtime cannot answer a guest differently about the same documented behaviour.
//!
//! There is no `fs`, `net`, `child_process`, `worker_threads` or `vm`: nothing here reaches a
//! host path, a host socket, a host process or a host thread, so a guest requiring one gets
//! the `MODULE_NOT_FOUND` a package that is not there gets.

/// The global the response sink is handed to the engine as.
pub(super) const HOST_RESPOND: &str = "__lambdaWebHostRespond";

/// The global the log writer is handed to the engine as.
pub(super) const HOST_LOG: &str = "__lambdaWebHostLog";

/// The global the runtime's own protocol object is held on, which is what the host drives a
/// request through and what the module resolver and loader ask.
pub(super) const RUNTIME_GLOBAL: &str = "__lambdaWebRuntime";

/// The expression a generated ES module reads a loaded module back from, which is the
/// accessor the shared registry is given.
pub(super) const MODULE_ACCESSOR: &str = "globalThis.__lambdaWebRuntime.module";

/// The runtime a web function's code runs inside. See this module's header.
pub(super) const RUNTIME_SOURCE: &str = r##"
(function (registrySource, coreSources) {
    var hostRespond = globalThis.__lambdaWebHostRespond;
    var hostLog = globalThis.__lambdaWebHostLog;
    delete globalThis.__lambdaWebHostRespond;
    delete globalThis.__lambdaWebHostLog;

    var stringify = JSON.stringify;
    var parse = JSON.parse;

    function toText(value) {
        if (value === undefined || value === null) { return ""; }
        if (typeof value === "string") { return value; }
        return String(value);
    }

    /* ---- the core modules, compiled ONCE each from the shared source: a module the
       runtime itself holds (the event emitter the server object model is built on) is the
       same object the guest's own `require` of it answers with ---- */
    var core = {};
    function moduleNotFound(request) {
        var missing = new Error("Cannot find module '" + toText(request) + "'");
        missing.code = "MODULE_NOT_FOUND";
        return missing;
    }
    function coreRequire(request) {
        var name = toText(request).replace(/^node:/, "");
        if (Object.prototype.hasOwnProperty.call(core, name)) { return core[name]; }
        if (Object.prototype.hasOwnProperty.call(coreSources, name)) {
            return loadCore(name, coreSources[name]);
        }
        throw moduleNotFound(request);
    }
    function loadCore(name, source) {
        var held = { exports: {} };
        /* Cached before the body runs, as a module system does, so two modules that require
           each other see a partial export rather than being compiled twice. */
        core[name] = held.exports;
        var compiled = new Function(
            "exports", "require", "module", "__filename", "__dirname", source
        );
        compiled.call(held.exports, held.exports, coreRequire, held,
                      "/var/runtime/" + name + ".js", "/var/runtime");
        core[name] = held.exports;
        return core[name];
    }
    Object.keys(coreSources).forEach(function (name) {
        if (!Object.prototype.hasOwnProperty.call(core, name)) {
            loadCore(name, coreSources[name]);
        }
    });
    var EventEmitter = core.events;
    var util = core.util;

    /* ---- the guest's clock: timers on a VIRTUAL clock, so nothing ever sleeps ---- */
    var clock = 0;
    var timers = [];
    var sequence = 0;
    function schedule(fn, delay, args, every) {
        if (typeof fn !== "function") { return null; }
        var wait = Number(delay);
        if (!(wait >= 0)) { wait = 0; }
        sequence += 1;
        var timer = { fn: fn, due: clock + wait, every: every ? wait : null,
                      args: args || [], live: true, order: sequence };
        timers.push(timer);
        return timer;
    }
    function clearTimer(timer) { if (timer && typeof timer === "object") { timer.live = false; } }
    function tick() {
        var earliest = null;
        for (var i = 0; i < timers.length; i++) {
            var timer = timers[i];
            if (!timer.live) { continue; }
            if (earliest === null || timer.due < earliest.due ||
                (timer.due === earliest.due && timer.order < earliest.order)) { earliest = timer; }
        }
        timers = timers.filter(function (held) { return held.live; });
        if (earliest === null) { return false; }
        if (earliest.due > clock) { clock = earliest.due; }
        if (earliest.every === null) { earliest.live = false; }
        else { earliest.due = clock + (earliest.every > 1 ? earliest.every : 1); }
        try { earliest.fn.apply(null, earliest.args); }
        catch (error) { hostLog("Uncaught " + util.inspect(error)); }
        return true;
    }

    /* ---- the console: every line goes to the host's collector, formatted the way Node
       formats one (the shared `util`) ---- */
    function write() {
        hostLog(util.format.apply(null, arguments));
    }

    /* ---- http: the server the code registers, and the one request it is handed ---- */
    var STATUS_CODES = {
        200: "OK", 201: "Created", 202: "Accepted", 204: "No Content", 301: "Moved Permanently",
        302: "Found", 303: "See Other", 304: "Not Modified", 307: "Temporary Redirect",
        308: "Permanent Redirect", 400: "Bad Request", 401: "Unauthorized", 403: "Forbidden",
        404: "Not Found", 405: "Method Not Allowed", 409: "Conflict", 410: "Gone",
        413: "Payload Too Large", 415: "Unsupported Media Type", 422: "Unprocessable Entity",
        429: "Too Many Requests", 500: "Internal Server Error", 501: "Not Implemented",
        502: "Bad Gateway", 503: "Service Unavailable", 504: "Gateway Timeout"
    };
    var servers = [];
    function ServerResponse() {
        EventEmitter.call(this);
        this.statusCode = 200;
        this.statusMessage = null;
        this.headersSent = false;
        this.finished = false;
        this.writableEnded = false;
        this._headers = [];
        this._chunks = [];
    }
    ServerResponse.prototype = Object.create(EventEmitter.prototype);
    ServerResponse.prototype.constructor = ServerResponse;
    ServerResponse.prototype.setHeader = function (name, value) {
        var wanted = toText(name).toLowerCase();
        var held = this._headers.filter(function (pair) {
            return pair[0].toLowerCase() !== wanted;
        });
        if (Array.isArray(value)) {
            value.forEach(function (one) { held.push([toText(name), toText(one)]); });
        } else {
            held.push([toText(name), toText(value)]);
        }
        this._headers = held;
        return this;
    };
    ServerResponse.prototype.getHeader = function (name) {
        var wanted = toText(name).toLowerCase();
        for (var i = 0; i < this._headers.length; i++) {
            if (this._headers[i][0].toLowerCase() === wanted) { return this._headers[i][1]; }
        }
        return undefined;
    };
    ServerResponse.prototype.getHeaders = function () {
        var held = {};
        this._headers.forEach(function (pair) { held[pair[0].toLowerCase()] = pair[1]; });
        return held;
    };
    ServerResponse.prototype.hasHeader = function (name) {
        return this.getHeader(name) !== undefined;
    };
    ServerResponse.prototype.removeHeader = function (name) {
        var wanted = toText(name).toLowerCase();
        this._headers = this._headers.filter(function (pair) {
            return pair[0].toLowerCase() !== wanted;
        });
        return this;
    };
    ServerResponse.prototype.writeHead = function (status, reason, headers) {
        this.statusCode = Number(status) || 200;
        var given = headers;
        if (reason && typeof reason === "object") { given = reason; }
        else if (typeof reason === "string") { this.statusMessage = reason; }
        if (given) {
            var self = this;
            Object.keys(given).forEach(function (name) { self.setHeader(name, given[name]); });
        }
        this.headersSent = true;
        return this;
    };
    ServerResponse.prototype.flushHeaders = function () { this.headersSent = true; return this; };
    ServerResponse.prototype.setTimeout = function () { return this; };
    ServerResponse.prototype.write = function (chunk, encoding, done) {
        if (chunk !== undefined && chunk !== null) { this._chunks.push(toText(chunk)); }
        if (typeof encoding === "function") { schedule(encoding, 0, [], false); }
        else if (typeof done === "function") { schedule(done, 0, [], false); }
        return true;
    };
    ServerResponse.prototype.end = function (chunk, encoding, done) {
        if (this.finished) { return this; }
        if (typeof chunk === "function") { done = chunk; chunk = undefined; }
        else if (typeof encoding === "function") { done = encoding; }
        if (chunk !== undefined && chunk !== null) { this.write(chunk); }
        this.finished = true;
        this.writableEnded = true;
        this.headersSent = true;
        hostRespond(this.statusCode, stringify(this._headers), this._chunks.join(""));
        if (typeof done === "function") { schedule(done, 0, [], false); }
        this.emit("finish");
        this.emit("close");
        return this;
    };
    function IncomingMessage(method, url, headers, body) {
        EventEmitter.call(this);
        this.method = method;
        this.url = url;
        this.httpVersion = "1.1";
        this.httpVersionMajor = 1;
        this.httpVersionMinor = 1;
        this.headers = headers;
        this.rawHeaders = [];
        var self = this;
        Object.keys(headers).forEach(function (name) {
            self.rawHeaders.push(name);
            self.rawHeaders.push(headers[name]);
        });
        this.complete = false;
        this.socket = { remoteAddress: null, localAddress: null, remotePort: null };
        this.connection = this.socket;
        this._body = body;
    }
    IncomingMessage.prototype = Object.create(EventEmitter.prototype);
    IncomingMessage.prototype.constructor = IncomingMessage;
    IncomingMessage.prototype.setEncoding = function () { return this; };
    IncomingMessage.prototype.resume = function () { return this; };
    IncomingMessage.prototype.pause = function () { return this; };
    IncomingMessage.prototype.setTimeout = function () { return this; };

    function Server(handler) {
        EventEmitter.call(this);
        this.listening = false;
        this._port = null;
        if (typeof handler === "function") { this.on("request", handler); }
        servers.push(this);
    }
    Server.prototype = Object.create(EventEmitter.prototype);
    Server.prototype.constructor = Server;
    Server.prototype.listen = function () {
        var self = this;
        var callback = null;
        for (var i = 0; i < arguments.length; i++) {
            if (typeof arguments[i] === "function") { callback = arguments[i]; }
            else if (typeof arguments[i] === "number") { this._port = arguments[i]; }
            else if (typeof arguments[i] === "string" && /^[0-9]+$/.test(arguments[i])) {
                this._port = Number(arguments[i]);
            } else if (typeof arguments[i] === "object" && arguments[i] &&
                       arguments[i].port !== undefined) { this._port = Number(arguments[i].port); }
        }
        this.listening = true;
        schedule(function () { self.emit("listening"); if (callback) { callback(); } }, 0, [], false);
        return this;
    };
    Server.prototype.address = function () {
        return this.listening ? { address: "127.0.0.1", family: "IPv4", port: this._port } : null;
    };
    Server.prototype.close = function (callback) {
        this.listening = false;
        if (typeof callback === "function") { schedule(callback, 0, [], false); }
        return this;
    };
    Server.prototype.setTimeout = function () { return this; };
    Server.prototype.unref = function () { return this; };
    Server.prototype.ref = function () { return this; };

    function createServer(options, handler) {
        var given = typeof options === "function" ? options : handler;
        return new Server(given);
    }
    core.http = {
        createServer: createServer,
        Server: Server,
        ServerResponse: ServerResponse,
        IncomingMessage: IncomingMessage,
        STATUS_CODES: STATUS_CODES,
        METHODS: ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]
    };

    /* ---- the process this code runs as: the revision's own environment, and nothing of
       the process this emulator runs in ---- */
    var environment = {};
    var processObject = {
        env: environment,
        argv: ["/var/lang/bin/node", "/var/task/index.js"],
        argv0: "node",
        platform: "linux",
        arch: "x64",
        pid: 1,
        version: "v0.0.0",
        versions: { node: "0.0.0" },
        execPath: "/var/lang/bin/node",
        cwd: function () { return "/var/task"; },
        uptime: function () { return clock / 1000; },
        hrtime: function () { return [Math.floor(clock / 1000), (clock % 1000) * 1000000]; },
        memoryUsage: function () { return { rss: 0, heapTotal: 0, heapUsed: 0, external: 0 }; },
        nextTick: function (fn) {
            schedule(fn, 0, Array.prototype.slice.call(arguments, 1), false);
        },
        on: function () { return this; },
        once: function () { return this; },
        off: function () { return this; },
        removeListener: function () { return this; },
        emitWarning: function (warning) { hostLog("Warning: " + util.inspect(warning)); },
        exit: function () { throw new Error("process.exit is not available to a web function"); },
        exitCode: 0,
        stdout: { write: function (chunk) { hostLog(toText(chunk)); return true; } },
        stderr: { write: function (chunk) { hostLog(toText(chunk)); return true; } }
    };
    core.process = processObject;
    core.os = {
        platform: function () { return "linux"; },
        arch: function () { return "x64"; },
        type: function () { return "Linux"; },
        hostname: function () { return "lambda-web"; },
        tmpdir: function () { return "/tmp"; },
        cpus: function () { return []; },
        EOL: "\n"
    };

    /* ---- the module registry: the SHARED resolution and loading of this package's own
       files, compiled here and created once the package is held ---- */
    var registryModule = (function () {
        var held = { exports: {} };
        (new Function("exports", "module", registrySource))(held.exports, held);
        return held.exports;
    })();
    var registry = null;

    globalThis.setTimeout = function (fn, delay) {
        return schedule(fn, delay, Array.prototype.slice.call(arguments, 2), false);
    };
    globalThis.setInterval = function (fn, delay) {
        return schedule(fn, delay, Array.prototype.slice.call(arguments, 2), true);
    };
    globalThis.setImmediate = function (fn) {
        return schedule(fn, 0, Array.prototype.slice.call(arguments, 1), false);
    };
    globalThis.clearTimeout = clearTimer;
    globalThis.clearInterval = clearTimer;
    globalThis.clearImmediate = clearTimer;
    globalThis.console = { log: write, info: write, debug: write, warn: write,
                           error: write, trace: write,
                           dir: write, group: write, groupEnd: function () {} };
    globalThis.URL = core.url.URL;
    globalThis.URLSearchParams = core.url.URLSearchParams;
    globalThis.process = processObject;
    globalThis.require = coreRequire;

    /* The server a request is handed to: the last one the code registered that is
       listening, because a web function fronts the server its code is serving with. */
    function listening() {
        for (var i = servers.length - 1; i >= 0; i--) {
            if (servers[i].listening) { return servers[i]; }
        }
        return null;
    }

    var driver = {
        /* Hold the package and the environment, and answer with the module Node would
           start it on, as the shared registry resolves it. */
        start: function (givenFiles, givenEnvironment, accessor) {
            var files = {};
            try { files = parse(givenFiles) || {}; } catch (error) { files = {}; }
            var given = {};
            try { given = parse(givenEnvironment) || {}; } catch (error) { given = {}; }
            Object.keys(given).forEach(function (name) { environment[name] = toText(given[name]); });
            if (environment.PORT === undefined) { environment.PORT = "8080"; }
            registry = registryModule.create({
                files: files,
                core: core,
                accessor: toText(accessor),
                taskRoot: "/var/task"
            });
            globalThis.require = registry.requireFrom("");
            var entry = registry.entry();
            if (!entry.entry) { return stringify({ entry: "" }); }
            /* What the code reads as its own command line: the interpreter and the module
               Node was pointed at, which is the entry the registry resolved. */
            processObject.argv = ["/var/lang/bin/node", "/var/task/" + entry.entry];
            return stringify(entry);
        },
        /* Start a CommonJS entry module, the way `node index.js` does. */
        requireEntry: function (path) { registry.load(toText(path), toText(path)); },
        /* Hand the one request the endpoint is serving to the server the code registered.
           `false` says the code is listening on nothing. */
        dispatch: function (method, url, headers, body) {
            var serving = listening();
            if (serving === null) { return false; }
            var sent = {};
            parse(headers).forEach(function (pair) { sent[pair[0]] = pair[1]; });
            var request = new IncomingMessage(method, url, sent, body);
            var response = new ServerResponse();
            schedule(function () {
                if (body !== undefined && body !== null && body.length > 0) {
                    request.emit("data", body);
                }
                request.complete = true;
                request.emit("end");
            }, 0, [], false);
            serving.emit("request", request, response);
            return true;
        },
        /* Run the earliest callback the guest's own clock has due. */
        tick: tick,
        /* What the engine's module loader asks, answered by the shared registry. */
        resolveModule: function (base, name) {
            return registry === null ? null : registry.resolve(base, name);
        },
        moduleSource: function (path) {
            return registry === null ? null : registry.source(toText(path));
        },
        /* What a generated re-export reads its module back from. */
        module: function (path) { return registry.load(toText(path), toText(path)); },
        /* The release the runtime identifier names, which `process.version` reports. */
        setNodeVersion: function (version) {
            var release = toText(version);
            processObject.version = "v" + release;
            processObject.versions = { node: release };
        }
    };
    return driver;
})
"##;
