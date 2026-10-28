#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wayland-client.h>
#include "ext-session-lock-client-protocol.h"

static struct ext_session_lock_manager_v1 *manager;

static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)data; (void)version;
    if (!strcmp(interface, "ext_session_lock_manager_v1"))
        manager = wl_registry_bind(registry, name, &ext_session_lock_manager_v1_interface, 1);
}

static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}

static void locked(void *data, struct ext_session_lock_v1 *lock) {
    (void)data; (void)lock;
    puts("locked");
    fflush(stdout);
}

static void finished(void *data, struct ext_session_lock_v1 *lock) {
    (void)data; (void)lock;
    puts("finished");
    fflush(stdout);
    _Exit(4);
}

int main(void) {
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) return 1;
    const struct wl_registry_listener registry_listener = {global, removed};
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &registry_listener, NULL);
    if (wl_display_roundtrip(display) < 0) return 3;
    if (!manager) {
        puts("unavailable");
        fflush(stdout);
        return 2;
    }
    struct ext_session_lock_v1 *lock = ext_session_lock_manager_v1_lock(manager);
    const struct ext_session_lock_v1_listener listener = {locked, finished};
    ext_session_lock_v1_add_listener(lock, &listener, NULL);
    if (wl_display_roundtrip(display) < 0) return 3;
    puts("requested");
    fflush(stdout);
    while (wl_display_dispatch(display) != -1) {}
    return 0;
}
