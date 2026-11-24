FROM scratch AS build
COPY artifact /app
FROM build AS stage
FROM scratch AS deliver
COPY --from=stage /app /bin/app
LABEL note="build"
