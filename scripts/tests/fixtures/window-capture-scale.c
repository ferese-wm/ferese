/* Native-pixel stripes distinguish a preserved backing buffer from a resample. */
#define _GNU_SOURCE
#include <stdint.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>
#include "xdg-shell-client-protocol.h"
#include "viewporter-client-protocol.h"

static struct wl_compositor *compositor;
static struct wl_subcompositor *subcompositor;
static struct wl_shm *shm;
static struct xdg_wm_base *wm;
static struct wp_viewporter *viewporter;
static struct wl_surface *surface;
static struct wl_buffer *buffer;
static int logical_width = 640;
static int logical_height = 480;

static void ping(void *data, struct xdg_wm_base *base, uint32_t serial) {
    (void)data;
    xdg_wm_base_pong(base, serial);
}
static const struct xdg_wm_base_listener wm_listener = {.ping = ping};

static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)data; (void)version;
    if (!strcmp(interface, "wl_compositor"))
        compositor = wl_registry_bind(registry, name, &wl_compositor_interface, 4);
    else if (!strcmp(interface, "wl_subcompositor"))
        subcompositor = wl_registry_bind(registry, name, &wl_subcompositor_interface, 1);
    else if (!strcmp(interface, "wl_shm"))
        shm = wl_registry_bind(registry, name, &wl_shm_interface, 1);
    else if (!strcmp(interface, "wp_viewporter"))
        viewporter = wl_registry_bind(registry, name, &wp_viewporter_interface, 1);
    else if (!strcmp(interface, "xdg_wm_base")) {
        wm = wl_registry_bind(registry, name, &xdg_wm_base_interface, 1);
        xdg_wm_base_add_listener(wm, &wm_listener, NULL);
    }
}
static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
static const struct wl_registry_listener registry_listener = {global, removed};

static void configure(void *data, struct xdg_surface *xdg, uint32_t serial) {
    (void)data;
    xdg_surface_ack_configure(xdg, serial);
    xdg_surface_set_window_geometry(xdg, 0, 0, logical_width, logical_height);
    wl_surface_attach(surface, buffer, 0, 0);
    wl_surface_damage_buffer(surface, 0, 0, INT32_MAX, INT32_MAX);
    wl_surface_commit(surface);
}
static const struct xdg_surface_listener surface_listener = {.configure = configure};
static void size(void *data, struct xdg_toplevel *top, int32_t w, int32_t h,
                 struct wl_array *states) {
    (void)data; (void)top; (void)w; (void)h; (void)states;
}
static void close_window(void *data, struct xdg_toplevel *top) {
    (void)data; (void)top; exit(0);
}
static const struct xdg_toplevel_listener top_listener = {.configure = size, .close = close_window};

int main(int argc, char **argv) {
    if (argc != 2) return 1;
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) return 2;
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &registry_listener, NULL);
    if (wl_display_roundtrip(display) < 0 || !compositor || !shm || !wm || !viewporter) return 3;
    int integer = !strcmp(argv[1], "integer");
    int fractional = !strcmp(argv[1], "fractional");
    int rounded = !strcmp(argv[1], "fractional-rounded");
    int ceiled = !strcmp(argv[1], "fractional-ceiled");
    int cropped = !strcmp(argv[1], "cropped");
    int workspace = !strcmp(argv[1], "workspace-native");
    if (rounded || ceiled) {
        logical_width = 2182;
        logical_height = 1195;
    }
    int width = rounded || ceiled ? 3819 : fractional ? 1120 : 1280;
    int height = rounded ? 2091 : ceiled ? 2092 : fractional ? 840 : 960;
    if (workspace) {
        const char *value = getenv("FERESE_TEST_OUTPUT_SCALE");
        char *end;
        if (!value) return 9;
        double scale = strtod(value, &end);
        if (*end || !(scale >= 0.25 && scale <= 8.0)) return 9;
        width = (int)(logical_width * scale + 0.5);
        height = (int)(logical_height * scale + 0.5);
    }
    int fd = memfd_create("window-capture-scale", MFD_CLOEXEC);
    size_t bytes = (size_t)width * height * 4;
    if (fd < 0 || ftruncate(fd, bytes)) return 4;
    uint32_t *pixels = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (pixels == MAP_FAILED) return 5;
    for (int y = 0; y < height; y++)
        for (int x = 0; x < width; x++)
            pixels[y * width + x] = (x + ((rounded || ceiled) ? y : 0)) % 2
                ? 0xffffffff : 0xff000000;
    struct wl_shm_pool *pool = wl_shm_create_pool(shm, fd, bytes);
    buffer = wl_shm_pool_create_buffer(pool, 0, width, height, width * 4, WL_SHM_FORMAT_ARGB8888);
    wl_shm_pool_destroy(pool);
    munmap(pixels, bytes);
    close(fd);
    surface = wl_compositor_create_surface(compositor);
    if (!strcmp(argv[1], "subsurface") || !strcmp(argv[1], "subsurface-outside")) {
        if (!subcompositor) return 6;
        struct wl_surface *child = wl_compositor_create_surface(compositor);
        struct wl_subsurface *sub = wl_subcompositor_get_subsurface(subcompositor, child, surface);
        if (!strcmp(argv[1], "subsurface-outside"))
            wl_subsurface_set_position(sub, 1280, 0);
        wl_surface_set_buffer_scale(child, 2);
        wl_surface_attach(child, buffer, 0, 0);
        wl_surface_damage_buffer(child, 0, 0, INT32_MAX, INT32_MAX);
        wl_surface_commit(child);
        // A transparent placeholder makes the child's density authoritative.
        int root_fd = memfd_create("capture-scale-root", MFD_CLOEXEC);
        if (root_fd < 0 || ftruncate(root_fd, 4)) return 7;
        uint32_t *root_pixels = mmap(NULL, 4, PROT_READ | PROT_WRITE, MAP_SHARED, root_fd, 0);
        if (root_pixels == MAP_FAILED) return 8;
        *root_pixels = 0;
        struct wl_shm_pool *root_pool = wl_shm_create_pool(shm, root_fd, 4);
        buffer = wl_shm_pool_create_buffer(root_pool, 0, 1, 1, 4, WL_SHM_FORMAT_ARGB8888);
        wl_shm_pool_destroy(root_pool);
        munmap(root_pixels, 4);
        close(root_fd);
    }
    if (integer) {
        wl_surface_set_buffer_scale(surface, 2);
    } else {
        struct wp_viewport *viewport = wp_viewporter_get_viewport(viewporter, surface);
        wp_viewport_set_destination(viewport, logical_width, logical_height);
        if (cropped)
            wp_viewport_set_source(viewport, wl_fixed_from_int(320), wl_fixed_from_int(240),
                                   wl_fixed_from_int(640), wl_fixed_from_int(480));
    }
    struct xdg_surface *xdg = xdg_wm_base_get_xdg_surface(wm, surface);
    xdg_surface_add_listener(xdg, &surface_listener, NULL);
    struct xdg_toplevel *top = xdg_surface_get_toplevel(xdg);
    xdg_toplevel_add_listener(top, &top_listener, NULL);
    xdg_toplevel_set_app_id(top, "ferese.test.capture-scale");
    xdg_toplevel_set_title(top, argv[1]);
    wl_surface_commit(surface);
    while (1) {
        wl_display_flush(display);
        struct pollfd fds[2] = {{wl_display_get_fd(display), POLLIN, 0}, {STDIN_FILENO, POLLIN, 0}};
        if (poll(fds, 2, -1) < 0) break;
        if ((fds[0].revents | fds[1].revents) & (POLLERR | POLLHUP | POLLNVAL)) break;
        if (fds[0].revents & POLLIN && wl_display_dispatch(display) < 0) break;
        if (fds[1].revents & POLLIN) {
            char command;
            if (read(STDIN_FILENO, &command, 1) != 1) break;
            if (command == 'd') {
                wl_surface_attach(surface, buffer, 0, 0);
                wl_surface_damage(surface, 0, 0, 640, 480);
                wl_surface_commit(surface);
            }
        }
    }
    return 0;
}
