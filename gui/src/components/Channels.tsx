import { CheckIcon } from "@heroicons/react/24/outline";
import { Disclosure, DisclosureGroup, ListBox, Spinner, Tabs } from "@heroui/react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { type JSX, useEffect, useState } from "react";

import { chibitvClient, queryKeys } from "../api";
import { useStream } from "../api/stream";
import { type Channel, DeliverySystem, type Service } from "../gen/chibitv/v1/chibitv_pb";

const DELIVERY_SYSTEMS: { id: DeliverySystem; label: string }[] = [
  { id: DeliverySystem.ISDB_T, label: "Terrestrial" },
  { id: DeliverySystem.ISDB_S, label: "BS 4K" },
  { id: DeliverySystem.UNSPECIFIED, label: "Other" },
];

interface ChannelsProps {
  onServiceChange?: () => void;
}

export function Channels({ onServiceChange }: ChannelsProps): JSX.Element {
  const { state, updateService } = useStream();
  const [expandedChannelId, setExpandedChannelId] = useState<number>();
  const [selectedDeliverySystem, setSelectedDeliverySystem] = useState<DeliverySystem>();
  const {
    data: services = [],
    isLoading: areServicesLoading,
    isError: areServicesError,
  } = useQuery({
    queryKey: queryKeys.services,
    queryFn: async () => (await chibitvClient.listServices({})).services,
    refetchInterval: (query) => (query.state.data?.length ? false : 1000),
  });
  const {
    data: channels = [],
    isLoading: areChannelsLoading,
    isError: areChannelsError,
  } = useQuery({
    queryKey: queryKeys.channels,
    queryFn: async () => (await chibitvClient.listChannels({})).channels,
  });
  const { mutate, variables, isPending } = useMutation({
    mutationFn: updateService,
    onSuccess: () => {
      onServiceChange?.();
    },
  });
  const serviceId = state?.service?.id;
  const currentChannelId = services.find((service) => service.id === serviceId)?.channelId;
  const currentDeliverySystem = channels.find((channel) => channel.id === currentChannelId)?.deliverySystem;
  const servicesByChannel = new Map<number, Service[]>();
  for (const service of services) {
    const channelServices = servicesByChannel.get(service.channelId) ?? [];
    channelServices.push(service);
    servicesByChannel.set(service.channelId, channelServices);
  }

  useEffect(() => {
    if (currentChannelId !== undefined) {
      setExpandedChannelId(currentChannelId);
    }
  }, [currentChannelId]);

  useEffect(() => {
    if (currentDeliverySystem !== undefined) {
      setSelectedDeliverySystem(currentDeliverySystem);
    }
  }, [currentDeliverySystem]);

  if (areServicesLoading || areChannelsLoading) {
    return (
      <div className="flex flex-1 items-center justify-center gap-3 text-sm text-muted">
        <Spinner size="sm" />
        Loading channels
      </div>
    );
  }

  if (areServicesError || areChannelsError) {
    return <p className="p-3 text-sm text-danger">Could not load channels.</p>;
  }

  const groups = DELIVERY_SYSTEMS.flatMap(({ id, label }) => {
    const groupChannels = channels.filter((channel) => channel.deliverySystem === id);

    return groupChannels.length === 0 ? [] : [{ id, label, channels: groupChannels }];
  });

  if (groups.length === 0) {
    return <p className="p-3 text-sm text-muted">No channels are available.</p>;
  }

  const renderChannels = (groupChannels: Channel[]) => (
    <DisclosureGroup
      className="gap-1"
      expandedKeys={expandedChannelId === undefined ? [] : [expandedChannelId]}
      onExpandedChange={(keys) => {
        const [key] = keys;
        if (key === undefined) {
          setExpandedChannelId(undefined);
          return;
        }

        const channelId = Number(key);
        setExpandedChannelId(channelId);

        const firstService = servicesByChannel.get(channelId)?.[0];
        if (firstService && firstService.id !== serviceId) {
          mutate(firstService.id);
        }
      }}
    >
      {groupChannels.map((channel) => {
        const channelServices = servicesByChannel.get(channel.id) ?? [];

        return (
          <Disclosure key={channel.id} id={channel.id} isDisabled={channelServices.length === 0 || isPending}>
            <Disclosure.Heading>
              <Disclosure.Trigger className="flex min-h-12 w-full flex-row items-center gap-2 rounded-xl px-3 text-sm font-semibold data-[expanded=true]:bg-accent-soft data-[expanded=true]:text-accent-soft-foreground">
                <span className="min-w-0 flex-1 truncate text-start">{channel.name}</span>
                <Disclosure.Indicator className="size-4 shrink-0" />
              </Disclosure.Trigger>
            </Disclosure.Heading>
            <Disclosure.Content>
              <Disclosure.Body className="pb-1 ps-3 pt-1">
                <ListBox
                  aria-label={`${channel.name} services`}
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
                      mutate(selectedServiceId);
                    }
                  }}
                >
                  {channelServices.map((service) => (
                    <ListBox.Item
                      key={service.id}
                      id={service.id}
                      className="min-h-12 rounded-xl px-3 data-[selected=true]:bg-accent-soft data-[selected=true]:text-accent-soft-foreground"
                      isDisabled={isPending}
                      textValue={service.name}
                    >
                      <div className="flex min-w-0 flex-1 flex-col">
                        <span className="truncate text-sm font-medium">{service.name}</span>
                        {service.providerName && (
                          <span className="truncate text-xs text-muted">{service.providerName}</span>
                        )}
                      </div>
                      {isPending && service.id === variables ? (
                        <Spinner className="ms-auto shrink-0" size="sm" />
                      ) : (
                        <ListBox.ItemIndicator className="text-accent">
                          <CheckIcon className="size-4" />
                        </ListBox.ItemIndicator>
                      )}
                    </ListBox.Item>
                  ))}
                </ListBox>
              </Disclosure.Body>
            </Disclosure.Content>
          </Disclosure>
        );
      })}
    </DisclosureGroup>
  );

  const selectedKey = groups.find((group) => group.id === selectedDeliverySystem)?.id ?? groups[0].id;

  return (
    <Tabs
      className="min-h-0 flex-1"
      selectedKey={selectedKey}
      onSelectionChange={(key) => setSelectedDeliverySystem(Number(key) as DeliverySystem)}
    >
      <Tabs.ListContainer className="shrink-0">
        <Tabs.List aria-label="Broadcast waves">
          {groups.map((group) => (
            <Tabs.Tab key={group.id} id={group.id}>
              <Tabs.Indicator />
              {group.label}
            </Tabs.Tab>
          ))}
        </Tabs.List>
      </Tabs.ListContainer>
      {groups.map((group) => (
        <Tabs.Panel key={group.id} id={group.id} className="min-h-0 flex-1 overflow-y-auto p-0">
          {renderChannels(group.channels)}
        </Tabs.Panel>
      ))}
    </Tabs>
  );
}
