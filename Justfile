# Copyright (c) 2023 Ho Kim (ho.kim@ulagbulag.io). All rights reserved.
# Use of this source code is governed by a GPL-3-style license that can be
# found in the LICENSE file.

# Load environment variables
set dotenv-load

# Configure environment variables
export DEBIAN_VERSION := env_var_or_default('DEBIAN_VERSION', 'bookworm')
export DEFAULT_RUNTIME_PACKAGE := env_var_or_default('DEFAULT_RUNTIME_PACKAGE', 'stressful-object-storage')
export OCI_BUILD_LOG_DIR := env_var_or_default('OCI_BUILD_LOG_DIR', './logs/')
export OCI_IMAGE := env_var_or_default('OCI_IMAGE', 'quay.io/ulagbulag/sos')
export OCI_IMAGE_VERSION := env_var_or_default('OCI_IMAGE_VERSION', 'latest')
export OCI_PLATFORMS := env_var_or_default('OCI_PLATFORMS', 'linux/arm64,linux/amd64')

default:
  @just run

init-conda:
  conda install --yes \
    -c pytorch -c nvidia \
    autopep8 pip python \
    pytorch torchvision torchaudio pytorch-cuda=11.8
  pip install -r ./requirements.txt

fmt:
  cargo fmt --all

build: fmt
  cargo build --all --workspace

clippy: fmt
  cargo clippy --all --workspace

test: clippy
  cargo test --all --workspace

run *ARGS:
  cargo run --package "${DEFAULT_RUNTIME_PACKAGE}" --release -- {{ ARGS }}

oci-build *ARGS:
  mkdir -p "${OCI_BUILD_LOG_DIR}"
  docker buildx build \
    --file './Dockerfile' \
    --tag "${OCI_IMAGE}:${OCI_IMAGE_VERSION}" \
    --build-arg DEBIAN_VERSION="${DEBIAN_VERSION}" \
    --platform "${OCI_PLATFORMS}" \
    --pull \
    {{ ARGS }} \
    . 2>&1 | tee "${OCI_BUILD_LOG_DIR}/build-base-$( date -u +%s ).log"

oci-push: (oci-build "--push")
