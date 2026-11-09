#include "shim.h"
#include "dl.hpp"
#include <cstddef>
#include <cstring>
#include <memory>
#include <stdexcept>

namespace {
// Мінімальний C ABI з Orca GUI/Printer/BambuTunnel.h. На Windows рядки там `wchar_t`
// (`tchar`), макети структур на x64 однакові (int, вказівник, unsigned long long).
#ifdef _WIN32
using tchar = wchar_t;
#else
using tchar = char;
#endif
struct StreamInfo {
    int type;
    int sub_type;
    union {
        struct { int width, height, frame_rate; } video;
        struct { int sample_rate, channel_count, sample_size; } audio;
    } format;
    int format_type, format_size, max_frame_size;
    const unsigned char *format_buffer;
};
struct Sample {
    int track, size, flags;
    const unsigned char *buffer;
    unsigned long long decode_time;
};
static_assert(sizeof(StreamInfo) == 40 && offsetof(StreamInfo, format_buffer) == 32);
static_assert(sizeof(Sample) == 32 && offsetof(Sample, buffer) == 16);
struct Camera {
    void *library = nullptr;
    void *tunnel = nullptr;
    int (*create)(void **, const tchar *) = nullptr;
    void (*logger)(void *, void (*)(void *, int, const tchar *), void *) = nullptr;
    int (*open)(void *) = nullptr;
    int (*start)(void *, bool) = nullptr;
    int (*info)(void *, int, StreamInfo *) = nullptr;
    int (*read)(void *, Sample *) = nullptr;
    void (*close)(void *) = nullptr;
    void (*destroy)(void *) = nullptr;
    template<class F> void load(F &fn, const char *name) {
        fn = reinterpret_cast<F>(dl::symbol(library, name));
        if (!fn) throw std::runtime_error("missing source symbol");
    }
    ~Camera() {
        if (tunnel) { close(tunnel); destroy(tunnel); }
        // Як у networking: код не вивантажується з-під запізнілих потоків SDK.
        if (library) dl::close(library);
    }
};
}
extern "C" int cloud_camera_open(const char *path, const char *url, void **out) {
    try {
        auto c = std::make_unique<Camera>();
        c->library = dl::open(path);
        if (!c->library) return -10001;
        c->load(c->create, "Bambu_Create");
        c->load(c->logger, "Bambu_SetLogger");
        c->load(c->open, "Bambu_Open");
        c->load(c->start, "Bambu_StartStream");
        c->load(c->info, "Bambu_GetStreamInfo");
        c->load(c->read, "Bambu_ReadSample");
        c->load(c->close, "Bambu_Close");
        c->load(c->destroy, "Bambu_Destroy");
#ifdef _WIN32
        std::wstring wide_url = dl::wide(url);
        int r = c->create(&c->tunnel, wide_url.c_str());
#else
        int r = c->create(&c->tunnel, url);
#endif
        if (r) return r;
        c->logger(c->tunnel, [](void *, int, const tchar *) {}, nullptr);
        r = c->open(c->tunnel);
        if (r) return r;
        *out = c.release();
        return 0;
    } catch (...) { return -10000; }
}
extern "C" int cloud_camera_start(void *handle) {
    try {
        auto &c = *static_cast<Camera *>(handle);
        int r = c.start(c.tunnel, true); // Лише відео; не файловий/керувальний канал.
        if (r) return r;
        StreamInfo info{};
        r = c.info(c.tunnel, 0, &info);
        if (r) return r;
        // VIDE=0, MJPG=1, video_jpeg=2. H.264 потребує окремого декодера.
        return info.type == 0 && info.sub_type == 1 && info.format_type == 2 ? 0 : -10007;
    } catch (...) { return -10000; }
}
extern "C" int cloud_camera_read(void *handle, CloudBytes *out, uint64_t *timestamp) {
    try {
        auto &c = *static_cast<Camera *>(handle);
        Sample sample{};
        int r = c.read(c.tunnel, &sample);
        if (r) return r;
        if (sample.track != 0) return 2; // Bambu_would_block: пропускаємо інші доріжки.
        if (!sample.buffer || sample.size <= 0 || sample.size > 16 * 1024 * 1024)
            return -10008;
        auto buffer = std::make_unique<uint8_t[]>(sample.size);
        std::memcpy(buffer.get(), sample.buffer, sample.size);
        out->data = buffer.release();
        out->len = sample.size;
        *timestamp = sample.decode_time;
        return 0;
    } catch (...) { return -10000; }
}
extern "C" void cloud_camera_close(void *handle) {
    delete static_cast<Camera *>(handle);
}
