#include "include/mini.h"

int hello_value(void) {
    return MINI_ANSWER;
}

int add_values(int left, int right) {
    return left + right;
}

void log_message(const char *tag, int level) {
    (void)tag;
    (void)level;
}

int main(void) {
    struct Point point = {1, 2};
    log_message("sum", add_values(point.x, point.y));
    return hello_value();
}
