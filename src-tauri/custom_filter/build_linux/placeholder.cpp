#include <iostream>

extern "C" {
    __attribute__((visibility("default")))
    void camlooper_placeholder() {
        std::cout << "CamLooper Custom Filter - Linux Placeholder" << std::endl;
    }
}
