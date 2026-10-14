// ABI fixture for an owned imported function inside a disposable client.
// Link against an owned dylib exporting orbit_probe_target with classic
// fixups. Requires com.parsec-orbit.hooks; does not hook vendor functions.
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>

typedef struct { uint32_t size, version; } Header;
typedef struct {
    Header header;
    const void *(*resolve)(const unsigned char *, size_t, const unsigned char *, size_t);
    int64_t (*install)(const void *, const void *, const void **);
    int32_t (*enable)(int64_t);
    int32_t (*disable)(int64_t);
    int32_t (*remove)(int64_t);
} Hooks;
typedef struct {
    uint32_t abi, size;
    void (*log)(uint32_t, const unsigned char *, size_t);
    void *(*query)(const unsigned char *);
} HostPrefix;
typedef struct {
    uint32_t abi, size;
    const unsigned char *id, *name, *version, *author, *description, *license;
    uint64_t capabilities;
} Descriptor;

extern int orbit_probe_target(int);
static int detour(int value) { return value + 1000; }
static void client_info(void *user, const unsigned char *json, size_t length) {
    const HostPrefix *host = user;
    if (json && length <= 4096) host->log(2, json, length);
}
static const Descriptor descriptor = {
    1, sizeof(Descriptor), (const unsigned char *)"com.example.hooks-probe",
    (const unsigned char *)"Owned imported-function probe",
    (const unsigned char *)"0.1.0", (const unsigned char *)"Orbit tests",
    (const unsigned char *)"Disposable client hook lifecycle", (const unsigned char *)"MIT", 0
};

const Descriptor *ParsecOrbitPluginQuery(uint32_t abi) {
    return abi == 1 ? &descriptor : NULL;
}

int32_t ParsecOrbitPluginInit(const HostPrefix *host) {
    if (!host || host->abi != 1 || !host->query || !host->log) return -1;
    typedef struct {
        Header header;
        int32_t (*info)(void (*)(void *, const unsigned char *, size_t), void *);
    } Client;
    const Client *client = host->query((const unsigned char *)"parsec-orbit.client");
    if (!client || client->header.size < sizeof(Client) || (client->header.version >> 16) != 1
            || !client->info || client->info(client_info, (void *)host) != 0) return -4;
    const Hooks *hooks = host->query((const unsigned char *)"parsec-orbit.hooks");
    if (!hooks || hooks->header.size < sizeof(Hooks) || (hooks->header.version >> 16) != 1) return -4;
    const char *module = "liborbit_probe_owned.dylib", *symbol = "orbit_probe_target";
    const void *target = hooks->resolve((const unsigned char *)module, strlen(module),
        (const unsigned char *)symbol, strlen(symbol));
    const void *original = NULL;
    int before = orbit_probe_target(7);
    int64_t id = hooks->install(target, (const void *)detour, &original);
    if (id <= 0 || !original) return -6;
    int32_t enable = hooks->enable(id);
    int hooked = orbit_probe_target(7);
    int unmodified = ((int (*)(int))original)(7);
    int32_t disable = hooks->disable(id);
    int disabled = orbit_probe_target(7);
    int32_t reenable = hooks->enable(id);
    int reenabled = orbit_probe_target(7);
    int32_t remove = hooks->remove(id);
    int removed = orbit_probe_target(7);
    char message[256];
    int length = snprintf(message, sizeof(message),
        "hooks-probe: before=%d enable=%d hooked=%d original=%d disable=%d disabled=%d reenable=%d reenabled=%d remove=%d removed=%d",
        before, enable, hooked, unmodified, disable, disabled, reenable, reenabled, remove, removed);
    if (length > 0 && (size_t)length < sizeof(message))
        host->log(2, (const unsigned char *)message, (size_t)length);
    return before == 8 && enable == 0 && hooked == 1007 && unmodified == 8
        && disable == 0 && disabled == 8 && reenable == 0 && reenabled == 1007
        && remove == 0 && removed == 8 ? 0 : -6;
}
