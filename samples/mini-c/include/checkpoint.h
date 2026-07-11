#pragma once


typedef struct AVTextWriter {
    const AVClass *priv_class;      ///< private class of the writer, if any
    int priv_size;                  ///< private size for the writer private class
    const char *name;

    int (*init)(AVTextWriterContext *wctx);
    int (*uninit)(AVTextWriterContext *wctx);
    void (*writer_w8)(AVTextWriterContext *wctx, int b);
    void (*writer_put_str)(AVTextWriterContext *wctx, const char *str);
    void (*writer_vprintf)(AVTextWriterContext *wctx, const char *fmt, va_list vl);
} AVTextWriter;