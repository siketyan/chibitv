import { CheckIcon } from "@heroicons/react/24/outline";
import { ListBox, Spinner } from "@heroui/react";
import type { JSX } from "react";

import { $api } from "../api";

interface ChannelsProps {
  onServiceChange?: () => void;
}

export function Channels({ onServiceChange }: ChannelsProps): JSX.Element {
  const { data: services = [], isLoading, isError } = $api.useQuery("get", "/services");
  const { data: stream = {}, refetch } = $api.useQuery("get", "/streams/{id}", {
    params: { path: { id: 0 } },
  });
  const { mutate, isPending } = $api.useMutation("patch", "/streams/{id}", {
    onSuccess: async () => {
      await refetch();
      onServiceChange?.();
    },
  });
  const serviceId = stream.service?.id;

  if (isLoading) {
    return (
      <div className="flex flex-1 items-center justify-center gap-3 text-sm text-muted">
        <Spinner size="sm" />
        Loading channels
      </div>
    );
  }

  if (isError) {
    return <p className="p-3 text-sm text-danger">Could not load channels.</p>;
  }

  if (services.length === 0) {
    return <p className="p-3 text-sm text-muted">No channels are available.</p>;
  }

  return (
    <ListBox
      aria-label="Channels"
      className="gap-1 p-0"
      selectedKeys={serviceId === undefined ? [] : [serviceId]}
      selectionMode="single"
      onSelectionChange={(keys) => {
        if (keys === "all") {
          return;
        }

        const [key] = keys;
        const selectedServiceId = Number(key);
        if (!Number.isNaN(selectedServiceId) && selectedServiceId !== serviceId) {
          mutate({ params: { path: { id: 0 } }, body: { service_id: selectedServiceId } });
        }
      }}
    >
      {services.map((service) => (
        <ListBox.Item
          key={service.id}
          id={service.id}
          className="min-h-12 rounded-xl px-3 data-[selected=true]:bg-accent-soft data-[selected=true]:text-accent-soft-foreground"
          isDisabled={isPending}
          textValue={service.name}
        >
          <div className="flex min-w-0 flex-1 flex-col">
            <span className="truncate text-sm font-medium">{service.name}</span>
            {service.provider_name && <span className="truncate text-xs text-muted">{service.provider_name}</span>}
          </div>
          {isPending && service.id === serviceId ? (
            <Spinner className="ms-auto shrink-0" size="sm" />
          ) : (
            <ListBox.ItemIndicator className="text-accent">
              <CheckIcon className="size-4" />
            </ListBox.ItemIndicator>
          )}
        </ListBox.Item>
      ))}
    </ListBox>
  );
}
