#pragma once
// Завантаження бібліотек плагіна: dlopen на Linux, LoadLibraryExW на Windows. В обох випадках
// бібліотека не вивантажується (RTLD_NODELETE / GET_MODULE_HANDLE_EX_FLAG_PIN): фонові потоки
// плагіна можуть завершуватися вже після destroy_agent.
#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#include <string>

namespace dl {
// Шлях з Rust приходить у UTF-8; Windows API приймає UTF-16.
inline std::wstring wide(const char *utf8) {
    int n = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, utf8, -1, nullptr, 0);
    if (n <= 0)
        return {};
    std::wstring out(static_cast<size_t>(n), L'\0');
    MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, utf8, -1, out.data(), n);
    out.resize(static_cast<size_t>(n - 1));
    return out;
}
inline void *open(const char *path) {
    std::wstring w = wide(path);
    if (w.empty())
        return nullptr;
    // Залежності плагіна (BambuSource.dll, бібліотеки agora) шукаються в його власній теці,
    // а не поруч зі slicenv.exe. Повний шлях — вимога LOAD_WITH_ALTERED_SEARCH_PATH.
    HMODULE module = LoadLibraryExW(w.c_str(), nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
    if (!module)
        return nullptr;
    // HMODULE — базова адреса образу, тож за нею модуль і закріплюється.
    HMODULE pinned = nullptr;
    GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_PIN | GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                       reinterpret_cast<LPCWSTR>(module), &pinned);
    return module;
}
inline void *symbol(void *library, const char *name) {
    return reinterpret_cast<void *>(GetProcAddress(static_cast<HMODULE>(library), name));
}
inline void close(void *library) { FreeLibrary(static_cast<HMODULE>(library)); }
} // namespace dl
#else
#include <dlfcn.h>

namespace dl {
inline void *open(const char *path) { return dlopen(path, RTLD_NOW | RTLD_LOCAL | RTLD_NODELETE); }
inline void *symbol(void *library, const char *name) { return dlsym(library, name); }
inline void close(void *library) { dlclose(library); }
} // namespace dl
#endif
