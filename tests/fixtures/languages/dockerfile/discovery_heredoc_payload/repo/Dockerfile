# syntax=docker/dockerfile:1
FROM alpine AS build
COPY <<'PAYLOAD' /example
FROM scratch AS phantom
PAYLOAD
FROM scratch AS final
COPY --from=build /example /example
