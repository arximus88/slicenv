// Незалежна модель C ABI BambuTunnel.h без мережі та справжніх зображень.
#include <chrono>
#include <cstring>
#include <cwchar>
#include <thread>
// Як у BambuTunnel.h: на Windows рядки — wchar_t.
#ifdef _WIN32
using tchar = wchar_t;
#define T(s) L##s
#define tstrstr std::wcsstr
#else
using tchar = char;
#define T(s) s
#define tstrstr std::strstr
#endif
struct Info {
    int type, subtype;
    union { struct { int width, height, fps; } video; int audio[3]; } format;
    int format_type, format_size, max_frame_size;
    const unsigned char *format_buffer;
};
struct Sample { int track, size, flags; const unsigned char *buffer; unsigned long long time; };
struct Tunnel {
    int starts = 0, reads = 0;
    // Лише заголовок для тесту власного парсера, без пікселів.
    unsigned char data[20] = {255,216,255,224,0,4,0,0,255,192,0,8,8,1,224,2,128,1,255,217};
};
#ifdef _WIN32
#define FAKE_API __declspec(dllexport)
#else
#define FAKE_API
#endif
extern "C" {
FAKE_API int Bambu_Create(void **out, const tchar *url) {
    if (tstrstr(url, T("bambu:///synthetic?token=private")) != url || !tstrstr(url, T("&device=SYNTHETIC12345678"))) return -97;
    *out = new Tunnel;
    return 0;
}
FAKE_API void Bambu_SetLogger(void *, void (*)(void *, int, const tchar *), void *) {}
FAKE_API int Bambu_Open(void *) { return 0; }
FAKE_API int Bambu_StartStream(void *p, bool video) {
    if (!video) return -96;
    return ++static_cast<Tunnel *>(p)->starts == 1 ? 2 : 0;
}
FAKE_API int Bambu_GetStreamInfo(void *, int track, Info *info) {
    if (track != 0) return -95;
    *info = {};
    info->subtype = 1;
    info->format_type = 2;
    return 0;
}
FAKE_API int Bambu_ReadSample(void *p, Sample *sample) {
    auto &t = *static_cast<Tunnel *>(p);
    if (++t.reads % 2) { std::this_thread::sleep_for(std::chrono::milliseconds(1)); return 2; }
    // Наступне читання змінює позичений буфер.
    t.data[16] = t.reads == 2 ? 128 : 64;
    *sample = {0,20,1,t.data,static_cast<unsigned long long>(t.reads) * 1000000};
    return 0;
}
FAKE_API void Bambu_Close(void *) {}
FAKE_API void Bambu_Destroy(void *p) { delete static_cast<Tunnel *>(p); }
}
