// Private resident adapter for transcribe.cpp. Kept in a separate process so
// its ggml symbols cannot collide with the app's whisper.cpp runtime.
// Protocol: little-endian u32 sample count, u32 UTF-8 prompt size, f32 PCM,
// prompt bytes. Reply: u32 status, u32 UTF-8 size, text. Zero status is success.
#include <transcribe.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <string>
#include <vector>

static bool read_exact(void *p, size_t n) { return std::fread(p, 1, n, stdin) == n; }
static bool read_u32(uint32_t &v) {
    unsigned char b[4];
    if (!read_exact(b, 4)) return false;
    v = uint32_t(b[0]) | uint32_t(b[1]) << 8 | uint32_t(b[2]) << 16 | uint32_t(b[3]) << 24;
    return true;
}
static void write_u32(uint32_t v) {
    unsigned char b[4] = {uint8_t(v), uint8_t(v >> 8), uint8_t(v >> 16), uint8_t(v >> 24)};
    std::fwrite(b, 1, 4, stdout);
}
static void reply(uint32_t status, const std::string &text) {
    write_u32(status);
    write_u32(uint32_t(text.size()));
    std::fwrite(text.data(), 1, text.size(), stdout);
    std::fflush(stdout);
}
static void log_message(transcribe_log_level, const char *message, void *) {
    std::fputs(message, stderr);
}

int main(int argc, char **argv) {
    if (argc != 5) return 2; // model, language, threads, GPU index
    transcribe_model *model = nullptr;
    transcribe_session *session = nullptr;
    try {
        transcribe_log_set(log_message, nullptr);
        if (transcribe_init_backends_default() != TRANSCRIBE_OK) return 3;
        transcribe_model_load_params mp;
        transcribe_model_load_params_init(&mp);
#ifdef __APPLE__
        mp.backend = TRANSCRIBE_BACKEND_METAL;
#else
        mp.backend = TRANSCRIBE_BACKEND_VULKAN;
#endif
        mp.device = transcribe_device_get(std::atoi(argv[4]));
        if (!mp.device) return 4;
        auto status = transcribe_model_load_file(argv[1], &mp, &model);
        if (status != TRANSCRIBE_OK) {
            reply(status, "Qwen model load failed");
            return 5;
        }
        transcribe_session_params sp;
        transcribe_session_params_init(&sp);
        sp.n_threads = std::atoi(argv[3]);
        status = transcribe_session_init(model, &sp, &session);
        if (status != TRANSCRIBE_OK) {
            reply(status, "Qwen session initialization failed");
            transcribe_model_free(model);
            return 6;
        }
        reply(0, "ready");
        uint32_t count, prompt_size;
        while (read_u32(count) && read_u32(prompt_size)) {
            if (!count || count > 16000U * 3600 || prompt_size > 65536) break;
            std::vector<float> pcm(count);
            std::string prompt(prompt_size, '\0');
            // Supported Linux/macOS targets are little-endian IEEE float32.
            if (!read_exact(pcm.data(), count * sizeof(float)) ||
                !read_exact(prompt.data(), prompt_size)) break;
            transcribe_run_params rp;
            transcribe_run_params_init(&rp);
            rp.timestamps = TRANSCRIBE_TIMESTAMPS_NONE;
            rp.language = std::strcmp(argv[2], "auto") == 0 ? nullptr : argv[2];
            rp.prompt = prompt.empty() ? nullptr : prompt.c_str();
            status = transcribe_run(session, pcm.data(), int(count), &rp);
            reply(status, status == TRANSCRIBE_OK ? transcribe_full_text(session) : "Qwen inference failed");
        }
    } catch (const std::exception &e) {
        reply(1, e.what());
    }
    transcribe_session_free(session);
    transcribe_model_free(model);
    return 0;
}
