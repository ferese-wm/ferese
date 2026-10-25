#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>
#include "xdg-shell-client-protocol.h"

static struct wl_compositor *compositor;
static struct wl_shm *shm;
static struct xdg_wm_base *wm;

static void ping(void *data, struct xdg_wm_base *base, uint32_t serial) {
    (void)data;
    xdg_wm_base_pong(base, serial);
}

static const struct xdg_wm_base_listener wm_listener = {.ping = ping};

static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)data;
    (void)version;
    if (!strcmp(interface, "wl_compositor"))
        compositor = wl_registry_bind(registry, name, &wl_compositor_interface, 4);
    else if (!strcmp(interface, "wl_shm"))
        shm = wl_registry_bind(registry, name, &wl_shm_interface, 1);
    else if (!strcmp(interface, "xdg_wm_base")) {
        wm = wl_registry_bind(registry, name, &xdg_wm_base_interface, 1);
        xdg_wm_base_add_listener(wm, &wm_listener, NULL);
    }
}

static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data;
    (void)registry;
    (void)name;
}

static const struct wl_registry_listener registry_listener = {global, removed};

static void configure(void *data, struct xdg_surface *xdg, uint32_t serial) {
    (void)data;
    xdg_surface_ack_configure(xdg, serial);
}

static const struct xdg_surface_listener surface_listener = {.configure = configure};

static void size(void *data, struct xdg_toplevel *top, int32_t width, int32_t height,
                 struct wl_array *states) {
    (void)data;
    (void)top;
    (void)width;
    (void)height;
    (void)states;
}

static void close_window(void *data, struct xdg_toplevel *top) {
    (void)data;
    (void)top;
    exit(0);
}

static const struct xdg_toplevel_listener top_listener = {
    .configure = size,
    .close = close_window,
};

int main(void) {
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) return 1;
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &registry_listener, NULL);
    if (wl_display_roundtrip(display) < 0 || !compositor || !shm || !wm) return 2;
    int fd = memfd_create("window-titles", MFD_CLOEXEC);
    if (fd < 0 || ftruncate(fd, 160 * 120 * 4)) return 3;
    uint32_t *pixels = mmap(NULL, 160 * 120 * 4, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (pixels == MAP_FAILED) return 4;
    for (size_t i = 0; i < 160 * 120; i++) pixels[i] = 0xff3182ce;
    struct wl_shm_pool *pool = wl_shm_create_pool(shm, fd, 160 * 120 * 4);
    struct wl_buffer *buffer = wl_shm_pool_create_buffer(pool, 0, 160, 120, 160 * 4, WL_SHM_FORMAT_XRGB8888);
    wl_shm_pool_destroy(pool);
    munmap(pixels, 160 * 120 * 4);
    close(fd);
    struct xdg_toplevel *tops[2];
    for (int i = 0; i < 2; i++) {
        struct wl_surface *window = wl_compositor_create_surface(compositor);
        struct xdg_surface *xdg = xdg_wm_base_get_xdg_surface(wm, window);
        xdg_surface_add_listener(xdg, &surface_listener, NULL);
        tops[i] = xdg_surface_get_toplevel(xdg);
        xdg_toplevel_add_listener(tops[i], &top_listener, NULL);
        xdg_toplevel_set_app_id(tops[i], "ferese.test.titles");
        xdg_toplevel_set_title(tops[i], i ? "Focused window" : "Background window");
        xdg_toplevel_set_min_size(tops[i], 160, 120);
        xdg_toplevel_set_max_size(tops[i], 160, 120);
        wl_surface_commit(window);
        if (wl_display_roundtrip(display) < 0) return 5;
        xdg_surface_set_window_geometry(xdg, 0, 0, 160, 120);
        wl_surface_attach(window, buffer, 0, 0);
        wl_surface_damage(window, 0, 0, 160, 120);
        wl_surface_commit(window);
        if (wl_display_roundtrip(display) < 0) return 5;
    }
    puts("ready"); fflush(stdout);
    char line[256];
    while (fgets(line, sizeof line, stdin)) {
        int index = line[0] - '0';
        if (index < 0 || index > 1 || line[1] != ' ') return 6;
        line[strcspn(line, "\n")] = 0;
        xdg_toplevel_set_title(tops[index], line + 2);
        if (wl_display_roundtrip(display) < 0) return 5;
        puts("changed"); fflush(stdout);
    }
    wl_display_disconnect(display);
    return 0;
}
