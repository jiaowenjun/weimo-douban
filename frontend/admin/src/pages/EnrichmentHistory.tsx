import { useEffect, useState } from 'react';
import { Alert, Input, Select, Space, Table, Tag } from 'antd';
import dayjs from 'dayjs';
import type { EnrichmentHistory as History } from '../types';
import { api } from '../api';
import { CATEGORY_OPTIONS } from '../labels';

const outcomes: Record<string, string> = {
  retained: '保留已有权威缓存',
  started: '执行中',
  interrupted: '执行中断，结果未知',
  cached: '已有缓存，未请求',
  success: '成功',
  retry: '失败，已安排重试',
  fallback: '降级完成',
  error: '尝试耗尽',
  not_found: '条目不存在',
};
export default function EnrichmentHistory({ updatedAt }: { updatedAt: number }) {
  const [category, setCategory] = useState('all');
  const [subject, setSubject] = useState('');
  const [page, setPage] = useState(1);
  const [data, setData] = useState<History | null>(null);
  const [error, setError] = useState('');
  useEffect(() => {
    let cancelled = false;
    api
      .enrichmentHistory(category, subject, page)
      .then((r) => {
        if (!cancelled) {
          setData(r);
          setError('');
        }
      })
      .catch((e) => {
        if (!cancelled) setError(String(e.message));
      });
    return () => {
      cancelled = true;
    };
  }, [category, subject, page, updatedAt]);
  return (
    <>
      {error && <Alert type="error" message={error} />}
      <Space style={{ marginBottom: 12 }}>
        <Select
          value={category}
          style={{ width: 100 }}
          options={[{ value: 'all', label: '全部' }, ...CATEGORY_OPTIONS]}
          onChange={(c) => {
            setCategory(c);
            setPage(1);
          }}
        />
        <Input.Search
          placeholder="按条目 ID 查看完整恢复过程"
          allowClear
          onSearch={(v) => {
            setSubject(v.trim());
            setPage(1);
          }}
          style={{ width: 280 }}
        />
      </Space>
      <Table
        size="small"
        rowKey="id"
        dataSource={data?.items}
        pagination={{
          current: page,
          total: data?.total,
          pageSize: 10,
          onChange: setPage,
          showSizeChanger: false,
        }}
        columns={[
          { title: '时间', render: (_, r) => dayjs(r.started_ms).format('MM-DD HH:mm:ss') },
          {
            title: '类别 / 条目',
            render: (_, r) =>
              `${CATEGORY_OPTIONS.find((c) => c.value === r.category)?.label ?? r.category} / ${r.subject_id}`,
          },
          { title: '第几次尝试', dataIndex: 'attempt' },
          {
            title: '结果',
            render: (_, r) => (
              <Tag color={r.recovered ? 'green' : r.outcome === 'retry' ? 'gold' : undefined}>
                {r.recovered ? '重试后恢复' : (outcomes[r.outcome] ?? r.outcome)}
              </Tag>
            ),
          },
          {
            title: 'HTTP / 原因',
            render: (_, r) => r.error ?? r.http_status ?? '—',
            ellipsis: true,
          },
        ]}
      />
    </>
  );
}
