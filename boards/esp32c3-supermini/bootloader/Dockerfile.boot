# Stock ESP-IDF second stage for ESP32-C3; externally signed together with
# the factory Ferriboot application using the provisioned RSA-3072 key.
FROM espressif/idf:v5.5.5 AS builder
SHELL ["/bin/bash", "-lc"]
WORKDIR /project
# The selected profile becomes the actual sdkconfig.defaults before IDF runs.
# Always generate sdkconfig fresh: never reuse a config from another profile.
ARG ESP_BOOTLOADER_PROFILE=dev
COPY bootloader/CMakeLists.txt bootloader/sdkconfig.defaults.dev bootloader/sdkconfig.defaults.release layout/partitions.csv ./

RUN case "$ESP_BOOTLOADER_PROFILE" in dev|release) ;; \
        *) echo "Invalid ESP_BOOTLOADER_PROFILE: $ESP_BOOTLOADER_PROFILE" >&2; exit 2 ;; \
    esac && \
    cp "sdkconfig.defaults.$ESP_BOOTLOADER_PROFILE" sdkconfig.defaults && \
    source "$IDF_PATH/export.sh" && \
    idf.py set-target esp32c3 && \
    idf.py bootloader && \
    test -s build/bootloader/bootloader.bin && \
    cp build/bootloader/bootloader.bin /bootloader.bin

FROM scratch AS artifact
COPY --from=builder /bootloader.bin /bootloader.bin
