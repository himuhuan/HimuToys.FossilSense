/* 🙂 */
int first(void), second(int value);
int a, b = 3;
int (*factory(void))(int), (*cb)(int);
int (*early_cb)(int), later_fn(void);
typedef int Function(int);
typedef int (*Callback)(int);
