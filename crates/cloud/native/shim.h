#pragma once
#include <stddef.h>
#include <stdint.h>

// Буфери належать шиму; callback позичає байти лише на час виклику.
struct CloudBytes {
    uint8_t *data;
    size_t len;
};
typedef void (*CloudCallback)(void *, int, const uint8_t *, size_t, const uint8_t *, size_t, int,
                              int);
extern "C" {
int cloud_open(const char *path, const char *log_dir, CloudCallback callback, void *user,
               void **out);
int cloud_close(void *handle);
// Версія плагіна без створення агента (для повідомлення «версія X не підтримується»).
int cloud_plugin_version(const char *path, CloudBytes *out);
void cloud_free(CloudBytes bytes);
int cloud_call(void *handle, int op, const char *a, const char *b, int number, CloudBytes *out);
// Поля: dev, task, project, preset, file, config, ams, ams2, ams_info, nozzle,
// nozzles_info, bed, extra.
// mode 0 — start_print (хмара), 1 — start_local_print (LAN: count >= 16, поля 13..15).
int cloud_print(void *handle, const char *const *fields, int count, int plate, uint32_t flags,
                int mode);
int cloud_camera_open(const char *path, const char *url, void **out);
int cloud_camera_start(void *handle);
int cloud_camera_read(void *handle, CloudBytes *out, uint64_t *timestamp);
void cloud_camera_close(void *handle);
}
