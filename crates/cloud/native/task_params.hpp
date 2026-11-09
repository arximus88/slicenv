#pragma once
#include <cstddef>
#include <string>

// Bambu Studio v02.06.00.51, bambu_networking.hpp / NetworkAgent.hpp.
namespace bambu_0206 {
struct TaskQueryParams {
    std::string dev_id;
    int status = 0;
    int offset = 0;
    int limit = 20;
};
static_assert(sizeof(TaskQueryParams) == 48);
static_assert(offsetof(TaskQueryParams, status) == 32);
static_assert(offsetof(TaskQueryParams, offset) == 36);
static_assert(offsetof(TaskQueryParams, limit) == 40);
}
