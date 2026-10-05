# 两个进程共用一份镜像；迁移由 sqlx::migrate! 编译期嵌进二进制，运行镜像里不需要 migrations/。
#
# API 按编译期绝对路径找前端产物（docs/operations/production.md §1.1）：构建与运行都在 /app，
# 产物放 /app/apps/web/dist；还要留 /app/apps/api 这个空目录，路径里的 ".." 才有得回退。

FROM node:24-bookworm-slim AS web
WORKDIR /app
COPY apps/web/package.json apps/web/package-lock.json apps/web/
RUN npm --prefix apps/web ci
COPY apps/web apps/web
RUN npm --prefix apps/web run build

FROM rust:1.96-slim-bookworm AS build
WORKDIR /app
# aws-lc-rs（reqwest 的 rustls 默认加密后端）需要 cmake 与 perl 才能从源码构建。
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake perl pkg-config \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p seeai-api -p seeai-worker

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /app/target/release/seeai-api /app/target/release/seeai-api
COPY --from=build /app/target/release/seeai-worker /app/target/release/seeai-worker
COPY --from=web /app/apps/web/dist /app/apps/web/dist
COPY --from=build /app/config/bootstrap /app/config/bootstrap
# 对客公开文档随镜像携带：容器里的 /v1/docs/*、素材导入解析 narrative_path 与直接运行一致。
COPY --from=build /app/public-docs /app/public-docs
RUN mkdir -p /app/apps/api \
    && useradd --uid 10001 --user-group --home-dir /app --shell /usr/sbin/nologin seeai
USER seeai
EXPOSE 8081
STOPSIGNAL SIGTERM
CMD ["/app/target/release/seeai-api"]
