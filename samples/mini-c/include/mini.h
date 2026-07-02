#ifndef MINI_H
#define MINI_H

#define MINI_ANSWER 42

struct Point {
    int x;
    int y;
};

enum Status {
    STATUS_OK,
    STATUS_BUSY,
};

int hello_value(void);
int add_values(int left, int right);
void log_message(const char *tag, int level);

#endif
