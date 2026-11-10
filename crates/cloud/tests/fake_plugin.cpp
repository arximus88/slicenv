// Локальна модель ABI: мережа та фізичні команди відсутні.
#include "../native/print_params.hpp"
#include "../native/task_params.hpp"
#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <iterator>
#include <sstream>
#include <chrono>
#include <fstream>
#include <functional>
#include <memory>
#include <string>
#include <thread>
#include <vector>
using Message = std::function<void(std::string, std::string)>;
struct Fake {
    std::shared_ptr<std::atomic<bool>> destroyed = std::make_shared<std::atomic<bool>>(false);
    Message message;
    std::function<void(int, int)> connected;
};
#ifdef FAKE_0208
using FakeParams = bambu_0208::PrintParams;
#else
using FakeParams = bambu_0206::PrintParams;
#endif
static std::atomic<bool> logged{true};
static std::string config_dir;
#ifdef _WIN32
#define FAKE_API __declspec(dllexport)
#else
#define FAKE_API
#endif
extern "C" {
// Макет за значенням: якщо покоління переплутане, поля після task_use_ams зсуваються.
FAKE_API int bambu_network_start_print(void *, FakeParams p, std::function<void(int, int, std::string)> up,
                              std::function<bool()>, std::function<bool(int, std::string)>) {
    if (p.dev_id != "SYNTHETIC12345678" || p.plate_index != 2 || !p.task_use_ams ||
        p.task_record_timelapse || p.task_bed_type != "textured_plate" ||
        p.extra_options != "extra" || p.connection_type != "cloud" ||
        p.extruder_cali_manual_mode != -1)
        return -98;
#ifdef FAKE_0208
    if (!p.svc_context.empty() || !p.slicer_uid.empty() || !p.queue_plate_id.empty())
        return -97;
#endif
    up(7, 0, "synthetic");
    return 0;
}
// LAN (start_local_print, без OnWaitFn): IP, код доступу, bblp, connection_type "lan".
FAKE_API int bambu_network_start_local_print(void *, FakeParams p,
                                             std::function<void(int, int, std::string)> up,
                                             std::function<bool()>) {
    if (p.dev_id != "SYNTHETIC12345678" || p.plate_index != 3 || p.dev_ip != "192.0.2.7" ||
        p.password != "syn-code" || p.username != "bblp" || p.connection_type != "lan" ||
        p.dev_name != "syn-printer" || !p.use_ssl_for_ftp || !p.use_ssl_for_mqtt ||
        !p.task_bed_leveling || p.task_use_ams || p.task_bed_type != "textured_plate" ||
        p.ams_mapping != "[-1]" || p.print_type != "from_normal" ||
        p.extruder_cali_manual_mode != -1)
        return -96;
#ifdef FAKE_0208
    if (!p.svc_context.empty() || !p.queue_plate_id.empty())
        return -95;
#endif
    up(1, 100, "");
    up(6, 0, "");
    return 0;
}
FAKE_API int bambu_network_get_my_profile(void *, std::string token, unsigned *code, std::string *body) {
    *code = token == "syn-access" ? 200 : 401;
    *body = R"({"uidStr":"42","name":"syn","account":"a@example.invalid","avatar":""})";
    return 0;
}
FAKE_API int bambu_network_get_my_token(void *, std::string ticket, unsigned *code, std::string *body) {
    *code = ticket == "syn-ticket" ? 200 : 400;
    *body = R"({"accessToken":"syn-access","refreshToken":"syn-refresh","expiresIn":7776000,"refreshExpiresIn":7776000})";
    return 0;
}
FAKE_API int bambu_network_change_user(void *, std::string info) {
    bool ok = info.find(R"("command":"user_login")") != std::string::npos &&
              info.find(R"("uid":"42")") != std::string::npos &&
              info.find(R"("token":"syn-access")") != std::string::npos &&
              info.find(R"("expires_in":"7776000.000000")") != std::string::npos;
    if (!ok)
        return -95;
    std::ofstream(config_dir + "/BambuNetworkEngine.conf") << "synthetic-login";
    logged = true;
    return 0;
}
FAKE_API int bambu_network_user_logout(void *, bool request) {
    if (request)
        return -96; // Вихід на сервері не повинен викликатися.
    logged = false;
    return 0;
}
FAKE_API std::string bambu_network_get_version() { return FAKE_VERSION; }
#ifdef FAKE_OBN_LOGGING
// Модель логування Open Bamboo Networking (abi_lifecycle.cpp, log.cpp, config.cpp):
// create_agent читає obn.conf з log_dir, а без нього пише шаблон із log_stderr = 1; змінні
// OBN_LOG_* мають пріоритет; налаштування застосовуються один раз. Значення вигадані.
static std::string conf_value(const std::string &text, const std::string &key) {
    std::string value, line;
    std::istringstream in(text);
    while (std::getline(in, line)) {
        if (line.empty() || line[0] == '#') continue;
        auto eq = line.find('=');
        if (eq == std::string::npos) continue;
        auto trim = [](std::string s) {
            s.erase(0, s.find_first_not_of(" \t"));
            s.erase(s.find_last_not_of(" \t\r") + 1);
            return s;
        };
        if (trim(line.substr(0, eq)) == key) value = trim(line.substr(eq + 1));
    }
    return value;
}
static bool truthy(const char *v) { return v && *v && std::string(v) != "0"; }
FAKE_API void *bambu_network_create_agent(std::string log_dir) {
    const std::string path = log_dir + "/obn.conf";
    std::string text;
    {
        std::ifstream in(path);
        if (in) text.assign(std::istreambuf_iterator<char>(in), {});
    }
    if (text.empty()) {
        text = "log_stderr = 1\nlog_to_file = 1\n";
        std::ofstream(path) << text;
    }
    bool echo = conf_value(text, "log_stderr") != "0";
    bool to_file = conf_value(text, "log_to_file") == "1";
    if (const char *e = std::getenv("OBN_LOG_STDERR")) echo = truthy(e);
    if (const char *e = std::getenv("OBN_LOG_TO_FILE")) to_file = truthy(e);
    if (std::getenv("OBN_LOG_FILE")) to_file = false;
    const char *line = "auth.cpp:124 load_locked: loaded session for synthetic-user@example.invalid "
                       "(user_id=999000111) printer SYNTHETIC12345678\n";
    if (echo) {
        std::fputs("[obn] ", stderr);
        std::fputs(line, stderr);
        std::fflush(stderr);
    }
    if (to_file) std::ofstream(log_dir + "/obn.log") << line;
    return new Fake;
}
#else
FAKE_API void *bambu_network_create_agent(std::string) { return new Fake; }
#endif
FAKE_API int bambu_network_destroy_agent(void *p) {
    auto agent = static_cast<Fake *>(p);
    agent->destroyed->store(true);
    delete agent;
    return 0;
}
FAKE_API int bambu_network_set_config_dir(void *, std::string dir) {
    config_dir = dir;
#ifdef FAKE_0208
    logged = std::ifstream(dir + "/BambuNetworkEngine.conf").good();
#endif
    std::ofstream(dir + "/test.marker") << "private-only";
    return 0;
}
FAKE_API int bambu_network_init_log(void *) { return 0; }
FAKE_API int bambu_network_set_country_code(void *, std::string) { return 0; }
FAKE_API int bambu_network_set_get_country_code_fn(void *, std::function<std::string()>) { return 0; }
FAKE_API int bambu_network_set_on_server_connected_fn(void *p, std::function<void(int, int)> fn) {
    static_cast<Fake *>(p)->connected = fn;
    return 0;
}
FAKE_API int bambu_network_set_on_message_fn(void *p, Message fn) {
    static_cast<Fake *>(p)->message = fn;
    return 0;
}
FAKE_API int bambu_network_set_on_user_message_fn(void *, Message) { return 0; }
FAKE_API int bambu_network_set_queue_on_main_fn(void *, std::function<void(std::function<void()>)>) {
    return 0;
}
FAKE_API int bambu_network_set_on_http_error_fn(void *, std::function<void(unsigned, std::string)>) { return 0; }
FAKE_API int bambu_network_track_enable(void *, bool) { return 0; }
FAKE_API bool bambu_network_is_user_login(void *) { return logged; }
FAKE_API bool bambu_network_is_server_connected(void *) { return true; }
FAKE_API int bambu_network_get_camera_url(void *, std::string id, std::function<void(std::string)> callback) {
    if (id != "SYNTHETIC12345678||\"tutk\",\"agora\"") return -94;
    std::thread([callback] { callback("bambu:///synthetic?token=private"); }).detach();
    return 0;
}
FAKE_API int bambu_network_get_user_tasks(void *, bambu_0206::TaskQueryParams p, std::string *body) {
    if (p.dev_id != "SYNTHETIC12345678" || p.offset != 7 || p.limit != 5 || p.status != 2)
        return -99;
    *body = R"({"hits":[{"id":101,"title":"private","deviceId":"SYNTHETIC12345678","status":2,"startTime":"2026-10-01T12:00:00Z","costTime":100,"weight":2.5,"plateIndex":1}]})";
    return 0;
}
FAKE_API int bambu_network_get_subtask_info(void *, std::string id, std::string *task, unsigned *code,
                                 std::string *body) {
    *code = id == "101" ? 0 : id == "102" ? 200 : 403;
    *task = R"({"id":101,"content":"private"})";
    *body = "private response";
    return 0;
}
FAKE_API int bambu_network_start(void *p) {
    auto fn = static_cast<Fake *>(p)->message;
    auto destroyed = static_cast<Fake *>(p)->destroyed;
    std::thread([fn, destroyed] {
        // Перший пакет живий; решта можуть прибути вже після destroy_agent.
        fn("SYNTHETIC12345678",
           R"({"print":{"command":"push_status","msg":0,"gcode_state":"IDLE","bed_temper":25}})");
        while (!destroyed->load())
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        for (int i = 0; i < 200; ++i)
            fn("SYNTHETIC12345678",
               R"({"print":{"command":"push_status","msg":1,"mc_percent":10}})");
    }).detach();
    return 0;
}
}
