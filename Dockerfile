FROM nvidia/cuda:12.8.0-cudnn-devel-ubuntu22.04 as builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    build-essential \
    curl \
    pkg-config \
    ca-certificates \
    gnupg \
    libssl-dev \
    openssl

# Install Rust
RUN curl https://sh.rustup.rs -sSf | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /app

# Copy project files
COPY . .

# Build the project
RUN cargo build --release --features gpu

# Runtime stage
FROM nvidia/cuda:12.8.0-cudnn-runtime-ubuntu22.04

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the binary and CUDA files from builder
COPY --from=builder /app/target/release/vanity ./
COPY --from=builder /app/kernels/*.cu /app/kernels/
COPY --from=builder /app/kernels/*.h /app/kernels/
COPY --from=builder /app/target/release/build /app/target/release/build

# Set environment variables
ENV NVIDIA_VISIBLE_DEVICES=all
ENV NVIDIA_DRIVER_CAPABILITIES=compute,utility

EXPOSE 8080

CMD ["./vanity"]
