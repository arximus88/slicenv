#include "shim.h"
#include "dl.hpp"
#include "print_params.hpp"
#include "task_params.hpp"
#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <cstring>
#include <functional>
#include <map>
#include <sstream>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <vector>

// Макети типів, що йдуть через межу за значенням, мусять збігатися з компілятором плагіна:
// GCC/libstdc++ (cxx11 ABI) на Linux і MSVC (/MD, release) на Windows. PrintParams містить лише
// std::string, int і bool, тож розмір і зсуви однакові в обох бібліотеках.
static_assert(sizeof(std::string) == 32);
static_assert(sizeof(std::vector<std::string>) == 24);
#ifdef _MSC_VER
static_assert(sizeof(std::function<void()>) == 64);
static_assert(_ITERATOR_DEBUG_LEVEL == 0, "плагін зібрано без налагоджувальних ітераторів");
#else
static_assert(sizeof(std::function<void()>) == 32);
#endif
static_assert(sizeof(bambu_0206::PrintParams) == 856);
static_assert(offsetof(bambu_0206::PrintParams, task_use_ams) == 766);
static_assert(offsetof(bambu_0206::PrintParams, task_bed_type) == 768);
static_assert(offsetof(bambu_0206::PrintParams, try_emmc_print) == 849);
static_assert(sizeof(bambu_0208::PrintParams) == 952);
static_assert(offsetof(bambu_0208::PrintParams, try_emmc_print) == 849);
static_assert(offsetof(bambu_0208::PrintParams, svc_context) == 856);
static_assert(offsetof(bambu_0208::PrintParams, queue_plate_id) == 920);

namespace {
constexpr int caught_exception = -10000;

// Покоління ABI (як `NetworkAbi` в Orca). Решта функцій, які викликає шим, у 02.06 і 02.08.04
// мають однакові сигнатури; відрізняється лише PrintParams. Дзеркало — `abi.rs`.
enum Abi { Unsupported = 0, V0206 = 1, V0208 = 2 };
Abi abi_of(const std::string &version) {
    // 02.06 — точна збірка, звірена з тегом Bambu Studio v02.06.00.51.
    if (version == "02.06.00.50")
        return V0206;
    // 02.08.04.xx: ABI стабільний у межах серії (правило Orca find_network_version_index).
    if (version.size() == 11 && version.compare(0, 9, "02.08.04.") == 0)
        return V0208;
    return Unsupported;
}
using Message = std::function<void(std::string, std::string)>;
using Connected = std::function<void(int, int)>;
using Update = std::function<void(int, int, std::string)>;

struct Gate {
    std::mutex mutex;
    CloudCallback callback = nullptr;
    void *user = nullptr;
    void emit(int kind, const std::string &a = {}, const std::string &b = {}, int x = 0,
              int y = 0) {
        std::lock_guard<std::mutex> lock(mutex);
        if (callback)
            callback(user, kind, reinterpret_cast<const uint8_t *>(a.data()), a.size(),
                     reinterpret_cast<const uint8_t *>(b.data()), b.size(), x, y);
    }
    void stop() {
        std::lock_guard<std::mutex> lock(mutex);
        callback = nullptr;
        user = nullptr;
    }
};

struct Agent {
    void *library = nullptr;
    void *agent = nullptr;
    std::string version;
    Abi abi = Unsupported;
    std::shared_ptr<Gate> gate;
    template <class F> F symbol(const char *name) {
        auto fn = reinterpret_cast<F>(dl::symbol(library, name));
        if (!fn)
            throw std::runtime_error("missing plugin symbol");
        return fn;
    }
    template <class R, class... A> R call(const char *name, A... args) {
        return symbol<R (*)(void *, A...)>(name)(agent, args...);
    }
    ~Agent() {
        // Код плагіна лишається в пам'яті: його фонові потоки можуть ще
        // завершуватися.
        if (library)
            dl::close(library);
    }
};
std::mutex lifecycle;
bool occupied = false;

void bytes(CloudBytes *out, const std::string &value) {
    auto p = std::make_unique<uint8_t[]>(value.size());
    std::memcpy(p.get(), value.data(), value.size());
    out->data = p.release();
    out->len = value.size();
}
}

extern "C" int cloud_open(const char *path, const char *log_dir, CloudCallback callback, void *user,
                          void **out) {
    std::lock_guard<std::mutex> lock(lifecycle);
    if (occupied)
        return -10003;
    try {
        auto h = std::make_unique<Agent>();
        h->library = dl::open(path);
        if (!h->library)
            return -10001;
        h->version = h->symbol<std::string (*)()>("bambu_network_get_version")();
        h->abi = abi_of(h->version);
        if (h->abi == Unsupported)
            return -10002;
        h->gate = std::make_shared<Gate>();
        h->gate->callback = callback;
        h->gate->user = user;
        // Перевіряємо базові символи до створення агента.
        h->symbol<int (*)(void *)>("bambu_network_destroy_agent");
        h->agent = h->symbol<void *(*)(std::string)>("bambu_network_create_agent")(log_dir);
        if (!h->agent)
            return -1;
        occupied = true;
        *out = h.release();
        return 0;
    } catch (...) {
        return caught_exception;
    }
}

extern "C" int cloud_plugin_version(const char *path, CloudBytes *out) {
    try {
        // Лише читання рядка версії; бібліотека лишається відображеною (RTLD_NODELETE), як і
        // при cloud_open, тож повторне завантаження не запускає конструктори вдруге.
        void *library = dl::open(path);
        if (!library)
            return -10001;
        auto fn = reinterpret_cast<std::string (*)()>(dl::symbol(library, "bambu_network_get_version"));
        if (!fn)
            return -10005;
        bytes(out, fn());
        return 0;
    } catch (...) {
        return caught_exception;
    }
}

extern "C" int cloud_close(void *handle) {
    std::lock_guard<std::mutex> lock(lifecycle);
    auto h = std::unique_ptr<Agent>(static_cast<Agent *>(handle));
    h->gate->stop(); // Чекаємо завершення Rust-callback перед звільненням user.
    try {
        int result = h->call<int>("bambu_network_destroy_agent");
        if (result == 0)
            occupied = false;
        return result;
    } catch (...) {
        return caught_exception;
    }
}

extern "C" void cloud_free(CloudBytes value) { delete[] value.data; }

extern "C" int cloud_call(void *handle, int op, const char *a, const char *b, int number,
                          CloudBytes *out) {
    auto &h = *static_cast<Agent *>(handle);
    try {
        switch (op) {
        case 0:
            bytes(out, h.version);
            return 0;
        case 1:
            return h.call<int>("bambu_network_set_config_dir", std::string(a));
        case 2:
            return h.call<int>("bambu_network_init_log");
        case 3: {
            std::string country(a);
            int r = h.call<int>("bambu_network_set_country_code", country);
            if (r)
                return r;
            return h.call<int>("bambu_network_set_get_country_code_fn",
                               std::function<std::string()>([country] { return country; }));
        }
        case 4: {
            auto gate = h.gate;
            int r = h.call<int>("bambu_network_set_on_server_connected_fn",
                                Connected([gate](int x, int y) { gate->emit(1, {}, {}, x, y); }));
            if (r)
                return r;
            r = h.call<int>(
                "bambu_network_set_on_message_fn",
                Message([gate](std::string dev, std::string msg) { gate->emit(2, dev, msg); }));
            if (r)
                return r;
            r = h.call<int>(
                "bambu_network_set_on_user_message_fn",
                Message([gate](std::string dev, std::string msg) { gate->emit(3, dev, msg); }));
            if (r)
                return r;
            r = h.call<int>("bambu_network_set_on_http_error_fn",
                std::function<void(unsigned, std::string)>([gate](unsigned code, std::string) {
                    gate->emit(5, {}, {}, static_cast<int>(code));
                }));
            if (r) return r;
            // Без GUI черга виконується на потоці плагіна; Rust лише кладе події в
            // канал.
            return h.call<int>(
                "bambu_network_set_queue_on_main_fn",
                std::function<void(std::function<void()>)>([](std::function<void()> fn) { fn(); }));
        }
        case 5:
            return h.call<int>("bambu_network_start");
        case 6:
            return h.call<int>("bambu_network_connect_server");
        case 7:
            return h.call<bool>("bambu_network_is_user_login") ? 1 : 0;
        case 8:
            bytes(out, h.call<std::string>("bambu_network_get_user_id"));
            return 0;
        case 9:
            bytes(out, h.call<std::string>("bambu_network_get_user_nickanme"));
            return 0;
        case 10: {
            unsigned code = 0;
            std::string body;
            int r = h.call<int>("bambu_network_get_user_print_info", &code, &body);
            if (r)
                return r;
            if (code != 200)
                return -11000 - static_cast<int>(code);
            bytes(out, body);
            return 0;
        }
        case 11:
            return h.call<int>("bambu_network_start_subscribe", std::string("app"));
        case 12:
            return h.call<int>("bambu_network_add_subscribe", std::vector<std::string>{a});
        case 13:
            return h.call<int>("bambu_network_send_message", std::string(a), std::string(b), number,
                               0);
        case 14:
            return h.call<bool>("bambu_network_is_server_connected") ? 1 : 0;
        case 15:
            return h.call<int>("bambu_network_set_cert_file", std::string(a), std::string(b));
        case 16:
            return h.call<int>("bambu_network_track_enable", false);
        case 17:
            return h.call<int>("bambu_network_set_user_selected_machine", std::string(a));
        case 18: {
            bambu_0206::TaskQueryParams params;
            params.dev_id = a;
            // Параметри надходять окремими десятковими цілими, без JSON-парсера C++.
            std::string fields(b);
            auto split = fields.find(':');
            if (split == std::string::npos) return -10004;
            params.status = std::stoi(fields.substr(0, split));
            params.limit = std::stoi(fields.substr(split + 1));
            params.offset = number;
            std::string body;
            int r = h.call<int>("bambu_network_get_user_tasks", params, &body);
            if (r) return r;
            bytes(out, body);
            return 0;
        }
        case 20: {
            // Callback може прибути після таймауту або destroy_agent: не позичає h/out.
            struct Reply {
                std::mutex mutex;
                std::condition_variable ready;
                bool done = false;
                std::string url;
            };
            auto reply = std::make_shared<Reply>();
            int r = h.call<int>("bambu_network_get_camera_url", std::string(a),
                std::function<void(std::string)>([reply](std::string url) {
                    std::lock_guard<std::mutex> lock(reply->mutex);
                    if (reply->done) return;
                    reply->url = std::move(url);
                    reply->done = true;
                    reply->ready.notify_one();
                }));
            if (r) return r;
            std::unique_lock<std::mutex> lock(reply->mutex);
            if (!reply->ready.wait_for(lock, std::chrono::seconds(20), [&] { return reply->done; })) {
                reply->done = true;
                return -10006;
            }
            if (reply->url.size() > 65536)
                return -10009;
            // Відмова плагіна — текст із кодом у кінці («…[N]»), як розбирає MediaPlayCtrl
            // Studio; Rust показує людині лише код, сам текст не логується.
            if (reply->url.rfind("bambu:///", 0) != 0) {
                bytes(out, reply->url);
                return -10010;
            }
            bytes(out, reply->url);
            return 0;
        }
        case 19: {
            unsigned code = 0;
            std::string task, body;
            int r = h.call<int>("bambu_network_get_subtask_info", std::string(a), &task,
                                &code, &body);
            if (r) return r;
            // У встановленому 02.06 успішний шлях може не заповнювати http_code.
            if (code != 0 && code != 200) return -11000 - static_cast<int>(code);
            if (task.empty()) return -10009;
            bytes(out, task);
            return 0;
        }
        case 21: {
            // Заголовки клієнта, як GUI_App::init_http_extra_header: рядки "ключ\nзначення\n…".
            std::map<std::string, std::string> headers;
            std::istringstream in{std::string(a)};
            std::string key, value;
            while (std::getline(in, key) && std::getline(in, value))
                headers[key] = value;
            return h.call<int>("bambu_network_set_extra_http_header", headers);
        }
        case 22:
            // Payload як у Orca HttpServer/WebUserLoginDialog: {"command":"user_login","data":{…}}.
            return h.call<int>("bambu_network_change_user", std::string(a));
        case 23:
            return h.call<int>("bambu_network_user_logout", number != 0);
        case 24:
        case 25: {
            unsigned code = 0;
            std::string body;
            int r = h.call<int>(op == 24 ? "bambu_network_get_my_profile" : "bambu_network_get_my_token",
                                std::string(a), &code, &body);
            if (r)
                return r;
            if (code != 0 && code != 200)
                return -11000 - static_cast<int>(code);
            bytes(out, body);
            return 0;
        }
        default:
            return -10004;
        }
    } catch (...) {
        return caught_exception;
    }
}

namespace {
template <class P> void fill(P &p, const char *const *f, int plate, uint32_t flags) {
    p.dev_id = f[0];
    p.task_name = f[1];
    p.project_name = f[2];
    p.preset_name = f[3];
    p.filename = f[4];
    p.config_filename = f[5];
    p.plate_index = plate;
    p.ams_mapping = f[6];
    p.ams_mapping2 = f[7];
    p.ams_mapping_info = f[8];
    p.nozzle_mapping = f[9];
    p.nozzles_info = f[10];
    p.task_bed_type = f[11];
    p.extra_options = f[12];
    p.connection_type = "cloud";
    p.print_type = "from_normal";
    p.use_ssl_for_ftp = true;
    p.use_ssl_for_mqtt = true;
    p.task_use_ams = flags & 1;
    p.task_record_timelapse = flags & 2;
    p.task_bed_leveling = flags & 4;
    p.task_flow_cali = flags & 8;
    p.task_vibration_cali = flags & 16;
    p.task_layer_inspect = flags & 32;
    p.task_timelapse_use_internal = flags & 64;
}
}

extern "C" int cloud_print(void *handle, const char *const *f, int plate, uint32_t flags) {
    auto &h = *static_cast<Agent *>(handle);
    try {
        auto gate = h.gate;
        Update update([gate](int stage, int code, std::string msg) { gate->emit(4, {}, msg, stage, code); });
        std::function<bool()> cancel([] { return false; });
        std::function<bool(int, std::string)> wait([](int, std::string) { return true; });
        // PrintParams передається за значенням: невірний макет псує стек, тому лише точне покоління.
        if (h.abi == V0208) {
            bambu_0208::PrintParams p{};
            fill(p, f, plate, flags);
            return h.call<int>("bambu_network_start_print", p, update, cancel, wait);
        }
        if (h.abi == V0206) {
            bambu_0206::PrintParams p{};
            fill(p, f, plate, flags);
            return h.call<int>("bambu_network_start_print", p, update, cancel, wait);
        }
        return -10002;
    } catch (...) {
        return caught_exception;
    }
}
