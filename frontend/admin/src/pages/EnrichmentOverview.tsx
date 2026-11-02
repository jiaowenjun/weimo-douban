import { Alert, Button, Card, Collapse, Popconfirm, Space, Table, Tag, Typography } from 'antd';
import dayjs from 'dayjs';
import EnrichmentHistory from './EnrichmentHistory';
import type { EnrichmentOverview as Overview } from '../types';

const labels: Record<string, string> = {
  movie: '电影',
  book: '图书',
  music: '音乐',
  '*': '全部',
  all: '全部',
};
const duration = (ms: number | null) => (ms == null ? '—' : `${Math.floor(ms / 60000)} 分钟`);

export default function EnrichmentOverview({
  data,
  busy,
  act,
}: {
  data: Overview | null;
  busy: boolean;
  act: (action: 'pause' | 'resume' | 'restore' | 'backfill', value?: string) => void;
}) {
  if (!data) return null;
  const paused = data.categories.some((c) => c.global_paused);
  const total = data.coverage.reduce((n, r) => n + r.total, 0);
  const enriched = data.coverage.reduce((n, r) => n + r.enriched, 0);
  return (
    <Card
      size="small"
      title="富集进展与控制"
      style={{ marginBottom: 16 }}
      extra={
        <Button
          loading={busy}
          disabled={!data.enabled}
          onClick={() => act(paused ? 'resume' : 'pause')}
        >
          {paused ? '恢复全部富集' : '暂停全部富集'}
        </Button>
      }
    >
      {!data.enabled && (
        <Alert
          type="warning"
          showIcon
          message="服务配置已关闭富集；恢复队列不能覆盖服务开关"
          style={{ marginBottom: 12 }}
        />
      )}
      <Typography.Paragraph type="secondary">
        按用户收藏计的权威信息覆盖率 {total ? `${((enriched / total) * 100).toFixed(1)}%` : '—'}。
        暂停保留任务和重试进度，重启后仍有效；已在执行的请求允许结束。 历史统计从{' '}
        {data.history_since_ms
          ? dayjs(data.history_since_ms).format('MM-DD HH:mm')
          : '启用新执行记录后'}{' '}
        开始，不追溯此前日志。
      </Typography.Paragraph>
      <Table
        size="small"
        rowKey="category"
        dataSource={data.categories}
        pagination={false}
        scroll={{ x: 1000 }}
        columns={[
          {
            title: '类别 / 状态',
            width: 145,
            render: (_, r) => (
              <Space style={{ whiteSpace: 'nowrap' }}>
                {labels[r.category]}
                <Tag
                  color={r.paused ? 'default' : r.blocked_until_ms > data.now_ms ? 'gold' : 'green'}
                >
                  {r.paused
                    ? '已暂停'
                    : r.blocked_until_ms > data.now_ms
                      ? '类别冷却'
                      : r.next_request_ms > data.now_ms
                        ? '间隔等待'
                        : '可调度'}
                </Tag>
              </Space>
            ),
          },
          { title: '首次等待 / 执行中', render: (_, r) => `${r.first_waiting} / ${r.running}` },
          { title: '等重试 / 已到期', render: (_, r) => `${r.retry_waiting} / ${r.retry_due}` },
          {
            title: '最长重试等待 / 到期延误',
            render: (_, r) => `${duration(r.retry_wait_ms)} / ${duration(r.dispatch_delay_ms)}`,
          },
          {
            title: '终态失败 / 降级',
            render: (_, r) =>
              `${r.errors} / ${r.history.filter((h) => h.outcome === 'fallback').reduce((n, h) => n + h.count, 0)}`,
          },
          {
            title: '累计成功 / 重试恢复 / 400',
            render: (_, r) =>
              [
                r.history.filter((h) => h.outcome === 'success'),
                r.history.filter((h) => h.recovered),
                r.history.filter((h) => h.http_status === 400),
              ]
                .map((list) => list.reduce((n, h) => n + h.count, 0))
                .join(' / '),
          },
          {
            title: '近一小时成功 / 400',
            render: (_, r) => `${r.last_hour.successes} / ${r.last_hour.http_400}`,
          },
          {
            title: '间隔 / 下限',
            render: (_, r) => `${r.interval_secs?.toFixed(1) ?? '—'} / ${r.floor_secs} 秒`,
          },
          {
            title: '操作',
            render: (_, r) => (
              <Button
                size="small"
                disabled={!data.enabled || r.global_paused}
                loading={busy}
                onClick={() => act(r.category_paused ? 'resume' : 'pause', r.category)}
              >
                {r.category_paused ? '恢复' : '暂停'}
              </Button>
            ),
          },
        ]}
      />
      <Collapse
        ghost
        style={{ marginTop: 12 }}
        items={[
          {
            key: 'coverage',
            label: '用户与类别覆盖缺口',
            children: (
              <>
                <Typography.Paragraph type="secondary">
                  订阅只覆盖对应用户；未排队缺口不会因其他用户的订阅而自动补齐。回填只补缺失任务，继续遵守暂停和冷却。
                </Typography.Paragraph>
                <Popconfirm
                  title="为全库缺失条目补回队列？"
                  description="包括未订阅用户的记录。不会重抓已有缓存，暂停状态保持不变。"
                  onConfirm={() => act('backfill')}
                >
                  <Button loading={busy} style={{ marginBottom: 12 }}>
                    显式回填全库缺口
                  </Button>
                </Popconfirm>
                <Table
                  size="small"
                  rowKey={(r) => `${r.uid}/${r.category}`}
                  dataSource={data.coverage}
                  pagination={{ pageSize: 8 }}
                  columns={[
                    { title: '用户 / 类别', render: (_, r) => `${r.uid} / ${labels[r.category]}` },
                    { title: '已富集 / 总数', render: (_, r) => `${r.enriched} / ${r.total}` },
                    { title: '降级缓存', dataIndex: 'fallback' },
                    {
                      title: '缺失 / 已排队 / 无任务',
                      render: (_, r) => `${r.missing} / ${r.queued} / ${r.unqueued}`,
                    },
                    {
                      title: '订阅',
                      render: (_, r) =>
                        r.subscribed ? (
                          `已启用 · ${r.next_full_ms ? dayjs(r.next_full_ms).format('MM-DD HH:mm') : '—'}`
                        ) : (
                          <Tag>未启用</Tag>
                        ),
                    },
                  ]}
                />
              </>
            ),
          },
          {
            key: 'history',
            label: '逐条执行历史（清空队列后仍保留）',
            children: <EnrichmentHistory updatedAt={data.now_ms} />,
          },
          {
            key: 'operations',
            label: '最近操作与清空快照',
            children: (
              <Table
                size="small"
                rowKey="id"
                dataSource={data.operations}
                pagination={{ pageSize: 6 }}
                columns={[
                  { title: '时间', render: (_, r) => dayjs(r.created_ms).format('MM-DD HH:mm:ss') },
                  {
                    title: '操作 / 范围',
                    render: (_, r) =>
                      `${({ clear: '清空', pause: '暂停', resume: '恢复执行', restore: '恢复快照', requeue: '重排', refresh: '刷新', backfill: '回填' } as Record<string, string>)[r.kind] ?? r.kind} / ${labels[r.category] ?? r.category}`,
                  },
                  {
                    title: '数量',
                    render: (_, r) =>
                      r.kind === 'clear'
                        ? `删除 ${r.counts.deleted ?? 0} · 等重试 ${r.counts.retrying ?? 0} · 在途 ${r.counts.in_flight ?? r.counts.running ?? 0}`
                        : (r.counts.restored ?? r.counts.queued ?? '—'),
                  },
                  {
                    title: '恢复',
                    render: (_, r) =>
                      r.kind === 'clear' && !r.restored_ms && !!r.counts.deleted ? (
                        <Popconfirm
                          title="恢复此快照中仍需要的任务？"
                          description="保留原重试状态；跳过已有任务、新结果和仍在执行的请求。"
                          onConfirm={() => act('restore', r.id)}
                        >
                          <Button size="small" loading={busy}>
                            恢复快照
                          </Button>
                        </Popconfirm>
                      ) : r.restored_ms ? (
                        '已恢复'
                      ) : (
                        '—'
                      ),
                  },
                ]}
              />
            ),
          },
        ]}
      />
    </Card>
  );
}
