/* Controlled resize client: acknowledge immediately, attach only on command.
 * Sync markers mean the server has processed the preceding ack/commit. */
#define _GNU_SOURCE
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>
#include "xdg-shell-client-protocol.h"

static struct wl_display *display;
static struct wl_compositor *compositor;
static struct wl_shm *shm;
static struct xdg_wm_base *wm;
static struct wl_surface *surface;
static struct xdg_surface *xdg;
static struct wl_buffer *buffer;
static int width = 64, height = 64, holding;
static uint32_t serial;

struct marker { const char *kind; uint32_t serial; int width, height; };
static void done(void *data, struct wl_callback *callback, uint32_t time) {
    (void)time;
    struct marker *m = data;
    printf("%s %u %d %d\n", m->kind, m->serial, m->width, m->height);
    fflush(stdout);
    wl_callback_destroy(callback);
    free(m);
}
static const struct wl_callback_listener sync_listener = {.done = done};
static void mark(const char *kind) {
    struct marker *m = malloc(sizeof(*m));
    if (!m) exit(3);
    *m = (struct marker){kind, serial, width, height};
    wl_callback_add_listener(wl_display_sync(display), &sync_listener, m);
}
static void attach(void) {
    size_t bytes = (size_t)width * height * 4;
    int fd = memfd_create("slow-resize", MFD_CLOEXEC);
    if (fd < 0 || ftruncate(fd, bytes)) exit(3);
    uint32_t *pixels = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (pixels == MAP_FAILED) exit(4);
    for (size_t i = 0; i < bytes / 4; i++) pixels[i] = 0xff3182ce;
    struct wl_shm_pool *pool = wl_shm_create_pool(shm, fd, bytes);
    struct wl_buffer *next = wl_shm_pool_create_buffer(pool, 0, width, height,
                                                       width * 4, WL_SHM_FORMAT_XRGB8888);
    wl_shm_pool_destroy(pool);
    munmap(pixels, bytes);
    close(fd);
    if (buffer) wl_buffer_destroy(buffer);
    buffer = next;
    xdg_surface_set_window_geometry(xdg, 0, 0, width, height);
    wl_surface_attach(surface, buffer, 0, 0);
    wl_surface_damage(surface, 0, 0, width, height);
    wl_surface_commit(surface);
    mark("committed");
}
static void configure(void *data, struct xdg_surface *shell, uint32_t value) {
    (void)data;
    serial = value;
    xdg_surface_ack_configure(shell, serial);
    if (holding) mark("held"); else attach();
}
static const struct xdg_surface_listener surface_listener = {.configure = configure};
static void size(void *data, struct xdg_toplevel *top, int32_t w, int32_t h, struct wl_array *states) {
    (void)data; (void)top; (void)states;
    if (w > 0) width = w;
    if (h > 0) height = h;
}
static void close_window(void *data, struct xdg_toplevel *top) {
    (void)data; (void)top; exit(0);
}
static const struct xdg_toplevel_listener top_listener = {.configure = size, .close = close_window};
static void ping(void *data, struct xdg_wm_base *base, uint32_t value) {
    (void)data; xdg_wm_base_pong(base, value);
}
static const struct xdg_wm_base_listener wm_listener = {.ping = ping};
static void global(void *data, struct wl_registry *registry, uint32_t name, const char *interface, uint32_t version) {
    (void)data; (void)version;
    if (!strcmp(interface, "wl_compositor")) compositor = wl_registry_bind(registry, name, &wl_compositor_interface, 4);
    else if (!strcmp(interface, "wl_shm")) shm = wl_registry_bind(registry, name, &wl_shm_interface, 1);
    else if (!strcmp(interface, "xdg_wm_base")) {
        wm = wl_registry_bind(registry, name, &xdg_wm_base_interface, 1);
        xdg_wm_base_add_listener(wm, &wm_listener, NULL);
    }
}
static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
static const struct wl_registry_listener registry_listener = {global, removed};
int main(int argc, char **argv) {
    display = wl_display_connect(NULL);
    if (!display) return 1;
    wl_registry_add_listener(wl_display_get_registry(display), &registry_listener, NULL);
    if (wl_display_roundtrip(display) < 0 || !compositor || !shm || !wm) return 2;
    surface = wl_compositor_create_surface(compositor);
    xdg = xdg_wm_base_get_xdg_surface(wm, surface);
    xdg_surface_add_listener(xdg, &surface_listener, NULL);
    struct xdg_toplevel *top = xdg_surface_get_toplevel(xdg);
    xdg_toplevel_add_listener(top, &top_listener, NULL);
    xdg_toplevel_set_app_id(top, argc > 1 ? argv[1] : "ferese.test.slow-resize");
    xdg_toplevel_set_title(top, "Controlled slow resize");
    wl_surface_commit(surface);
    for (;;) {
        if (wl_display_dispatch_pending(display) < 0 || wl_display_flush(display) < 0) return 5;
        struct pollfd fds[] = {{wl_display_get_fd(display), POLLIN, 0}, {STDIN_FILENO, POLLIN, 0}};
        if (poll(fds, 2, -1) < 0) return 6;
        if (fds[0].revents && wl_display_dispatch(display) < 0) return 5;
        if (fds[1].revents) {
            char command;
            if (read(STDIN_FILENO, &command, 1) != 1 || command == 'q') break;
            if (command == 'h') { holding = 1; mark("holding"); }
            if (command == 'c') attach();
        }
    }
    if (buffer) wl_buffer_destroy(buffer);
    wl_display_disconnect(display);
    return 0;
}
